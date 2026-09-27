//! A paste of files from the clipboard, written into the focused pane's
//! directory and then named there so the paste still types something.
//!
//! The clipboard's file list is preferred to its text: a copied file carries
//! both, and the text is only the path on this machine. The host writes the
//! bytes. This module asks first, then sends one file at a time.

use std::path::PathBuf;

use gpui_kit::{ClipboardEntry, ClipboardItem, Context, ExternalPaths};

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::ErrorCode;

use crate::bridge::EngineEvent;
use crate::input::TerminalInput;
use crate::prompt;
use crate::surface::FilePaste;
use crate::upload_tree::{self, Item};
use crate::vt::{PaneKey, VtCommand};
use crate::window::WindowShell;

pub use crate::upload_log::{UploadPhase, UploadRecord};

/// How many bytes a kibibyte holds.
const KIBIBYTE: u64 = 1024;
/// How many bytes a mebibyte holds.
const MEBIBYTE: u64 = KIBIBYTE.saturating_mul(KIBIBYTE);
/// A full progress reading, as a count out of this many.
pub(crate) const FULL_PERCENT: u16 = 100;

/// Files still being written into one pane, and every paste this window has seen.
#[derive(Clone, Debug, Default)]
pub struct Pending {
    /// The pane, while a paste is in progress.
    key: Option<PaneKey>,
    /// Every paste, newest last. Sending, waiting, finished and failed.
    pub(crate) records: Vec<UploadRecord>,
    /// Whether the uploads window has already been opened for this process.
    opened: bool,
    /// Whether a file has been handed to the host since this process started.
    live: bool,
    /// Whether the record on disk has been read.
    loaded: bool,
}

/// Bytes as a short reading: kibibytes until a mebibyte, then mebibytes.
#[must_use]
pub fn byte_text(bytes: u64) -> String {
    if bytes < KIBIBYTE {
        return format!("{bytes} B");
    }
    if bytes < MEBIBYTE {
        return format!("{} KiB", bytes.checked_div(KIBIBYTE).unwrap_or(0));
    }
    format!("{} MiB", bytes.checked_div(MEBIBYTE).unwrap_or(0))
}

/// How far `sent` is through `total`, from zero through [`FULL_PERCENT`].
///
/// An empty file is already complete.
#[must_use]
pub fn percent(sent: u64, total: u64) -> u16 {
    if total == 0 {
        return FULL_PERCENT;
    }
    let wide = sent
        .saturating_mul(u64::from(FULL_PERCENT))
        .checked_div(total)
        .unwrap_or(0);
    u16::try_from(wide).unwrap_or(FULL_PERCENT)
}

/// What a clipboard offers a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardPaste {
    /// One or more files.
    Files(Vec<PathBuf>),
    /// Text, when the clipboard has no files.
    Text(Option<String>),
}

/// The files on a clipboard item, or its text when there are none.
///
/// A copied file carries both a path list and a string. The string is the
/// path on this machine, which a remote pane cannot open, so the paths win.
#[must_use]
pub fn paste_clipboard(item: &ClipboardItem) -> ClipboardPaste {
    let mut paths = Vec::new();
    for entry in item.entries() {
        if let ClipboardEntry::ExternalPaths(external) = entry {
            paths.extend(file_list(external));
        }
    }
    if paths.is_empty() {
        ClipboardPaste::Text(item.text())
    } else {
        ClipboardPaste::Files(paths)
    }
}

/// The paths an external-paths entry holds.
fn file_list(external: &ExternalPaths) -> Vec<PathBuf> {
    external.paths().to_vec()
}

/// Quotes `path` so a shell will read it as one word, and leaves a trailing
/// space so the next paste, or the person's typing, is separate from it.
#[must_use]
pub fn shell_quote(path: &str) -> String {
    let windows = path.contains('\\') || path.as_bytes().get(1) == Some(&b':');
    if windows {
        let escaped = path.replace('"', "\\\"");
        return format!("\"{escaped}\" ");
    }
    let plain = path.bytes().all(|byte| {
        matches!(
            byte,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'.'
                | b'_'
                | b'-'
                | b'/'
                | b'@'
                | b'+'
                | b','
                | b':'
                | b'%'
        )
    });
    if plain {
        format!("{path} ")
    } else {
        format!("'{}' ", path.replace('\'', "'\\''"))
    }
}

impl WindowShell {
    /// Ask before uploading the files, or say why they will not be sent.
    pub fn confirm_upload(&mut self, paste: &FilePaste, context: &mut Context<'_, Self>) {
        ensure_loaded(self);
        let files = match upload_tree::collect(&paste.paths) {
            Ok(files) => files,
            Err(detail) => {
                self.failure(&paste.key.host, detail, context);
                return;
            }
        };
        if incomplete(self) {
            self.failure(
                &paste.key.host,
                "a file is already being uploaded".to_owned(),
                context,
            );
            return;
        }
        if !self.hosts().state().accepts_upload(&paste.key.host) {
            self.failure(
                &paste.key.host,
                "this host cannot receive a pasted file until its server is upgraded".to_owned(),
                context,
            );
            return;
        }
        if self.palette.prompt.is_some() {
            self.failure(
                &paste.key.host,
                "the files were not sent: another question is open; answer it, then paste again"
                    .to_owned(),
                context,
            );
            return;
        }
        let directory = pane_directory(self, &paste.key);
        self.palette
            .ask(prompt::upload_prompt(paste.key.clone(), files, directory));
        context.notify();
    }
}

/// Starts the upload a person confirmed.
///
/// # Errors
///
/// The bridge error when the host will not take the first file.
pub fn begin(
    shell: &mut WindowShell,
    key: PaneKey,
    files: &[Item],
) -> Result<(), crate::bridge::EngineError> {
    ensure_loaded(shell);
    let kept = std::mem::take(&mut shell.pending_upload.records);
    let opened = shell.pending_upload.opened;
    shell.pending_upload = Pending {
        key: Some(key),
        records: kept,
        opened,
        live: false,
        loaded: true,
    };
    remember(shell, files);
    let result = send_next(shell);
    write_record(shell);
    result
}

/// Opens the uploads window the first time a paste is under way.
pub fn open_once(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    if shell.pending_upload.opened {
        return;
    }
    shell.pending_upload.opened = true;
    crate::upload_window::open(context);
}

/// Opens the uploads window from the menu, even when one is already open.
pub fn show(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    ensure_loaded(shell);
    shell.pending_upload.opened = true;
    crate::upload_window::open(context);
}

/// Moves a finished or failed upload along, and ignores events about other work.
pub fn on_event(
    shell: &mut WindowShell,
    event: &EngineEvent,
    context: &mut Context<'_, WindowShell>,
) {
    ensure_loaded(shell);
    let EngineEvent::Said(said) = event else {
        return;
    };
    match said {
        ManagerEvent::UploadProgress {
            host,
            name,
            sent,
            total,
            ..
        } => {
            note_progress(shell, host, name, *sent, *total);
            open_once(shell, context);
        }
        ManagerEvent::UploadFinished { host, pane, path } => {
            if !same_pane(shell, host, *pane) {
                return;
            }
            let type_path = finish_record(shell, host, path);
            if type_path {
                let key = pane_key(host, *pane);
                let quoted = shell_quote(path);
                if shell
                    .thread
                    .send(VtCommand::Input {
                        key,
                        input: TerminalInput::ConfirmedPaste(quoted),
                    })
                    .is_err()
                {
                    stop(shell);
                    return;
                }
            }
            if let Err(error) = send_next(shell) {
                let host = host.clone();
                stop(shell);
                shell.failure(&host, error.to_string(), context);
            }
            write_record(shell);
        }
        ManagerEvent::UploadFailed { host, pane, detail } => {
            if held_for_later(detail) && same_pane(shell, host, *pane) {
                shell.pending_upload.live = false;
                write_record(shell);
                open_once(shell, context);
                return;
            }
            fail_records(shell, host, detail);
            if same_pane(shell, host, *pane) {
                stop(shell);
            }
            open_once(shell, context);
            shell.failure(host, detail.clone(), context);
            write_record(shell);
        }
        ManagerEvent::Notify(iznik_client::reduce::Notification::Refused {
            code: ErrorCode::Upload,
            host,
            message,
        }) if shell
            .pending_upload
            .key
            .as_ref()
            .is_some_and(|key| key.host == *host) =>
        {
            if let Some(at) = written_at(message)
                && continue_from(shell, host, at)
            {
                if let Err(error) = send_next(shell) {
                    let host = host.clone();
                    stop(shell);
                    shell.failure(&host, error.to_string(), context);
                }
                write_record(shell);
                return;
            }
            fail_records(shell, host, message);
            stop(shell);
            open_once(shell, context);
            write_record(shell);
        }
        _other => {}
    }
}

/// Records each file, the first as sending and the rest as waiting.
fn remember(shell: &mut WindowShell, files: &[Item]) {
    let Some(key) = shell.pending_upload.key.clone() else {
        return;
    };
    let mut first = true;
    for item in files {
        let total = if item.directory {
            0
        } else {
            std::fs::metadata(&item.local).map_or(0, |metadata| metadata.len())
        };
        let phase = if first {
            first = false;
            UploadPhase::Sending
        } else {
            UploadPhase::Waiting
        };
        shell.pending_upload.records.push(UploadRecord {
            name: item.name.clone(),
            host: key.host.0.clone(),
            pane: key.pane,
            local: item.local.clone(),
            sent: 0,
            total,
            remote: None,
            detail: None,
            phase,
            type_path: item.type_path,
            directory: item.directory,
            retry_once: false,
        });
    }
}

/// Moves `name` on `host` to sending and records how far it has gone.
fn note_progress(shell: &mut WindowShell, host: &HostId, name: &str, sent: u64, total: u64) {
    let Some(index) = shell.pending_upload.records.iter().rposition(|record| {
        record.host == host.0
            && record.name == name
            && matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting)
    }) else {
        return;
    };
    let Some(record) = shell.pending_upload.records.get_mut(index) else {
        return;
    };
    record.sent = sent;
    record.total = total;
    record.phase = UploadPhase::Sending;
    write_record(shell);
}

/// Marks the sending file finished and names the path the host gave it.
///
/// The returned flag says whether that path is typed into the pane.
fn finish_record(shell: &mut WindowShell, host: &HostId, path: &str) -> bool {
    let Some(record) = shell
        .pending_upload
        .records
        .iter_mut()
        .rev()
        .find(|record| record.phase == UploadPhase::Sending && record.host == host.0)
    else {
        return false;
    };
    let type_path = record.type_path;
    record.phase = UploadPhase::Finished;
    record.sent = record.total;
    record.remote = Some(path.to_owned());
    type_path
}

/// Marks the file in flight, and any still waiting, as failed.
fn fail_records(shell: &mut WindowShell, host: &HostId, detail: &str) {
    for record in &mut shell.pending_upload.records {
        if record.host != host.0 {
            continue;
        }
        if matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting) {
            record.phase = UploadPhase::Failed;
            record.detail = Some(detail.to_owned());
        }
    }
}

/// Whether `pane` on `host` is the pane an upload is being written for.
fn same_pane(shell: &WindowShell, host: &HostId, pane: PaneId) -> bool {
    shell
        .pending_upload
        .key
        .as_ref()
        .is_some_and(|key| key.host == *host && key.pane == pane)
}

/// The pane an event named.
fn pane_key(host: &HostId, pane: PaneId) -> PaneKey {
    PaneKey {
        host: host.clone(),
        pane,
    }
}

/// Sends the first file still queued, or finishes when none are.
///
/// # Errors
///
/// The bridge error when the host will not take the file.
fn send_next(shell: &mut WindowShell) -> Result<(), crate::bridge::EngineError> {
    let Some(index) = shell
        .pending_upload
        .records
        .iter()
        .position(|record| matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting))
    else {
        shell.pending_upload.key = None;
        shell.pending_upload.live = false;
        return Ok(());
    };
    let Some(record) = shell.pending_upload.records.get_mut(index) else {
        return Ok(());
    };
    record.phase = UploadPhase::Sending;
    let name = record.name.clone();
    let local = record.local.clone();
    let offset = if record.directory { 0 } else { record.sent };
    let pane = record.pane;
    let host = record.host.clone();
    shell.pending_upload.key = Some(PaneKey {
        host: HostId(host.clone()),
        pane,
    });
    match shell
        .hosts
        .bridge()
        .upload_file(&host, pane, name, local, offset)
    {
        Ok(()) => {
            shell.pending_upload.live = true;
            Ok(())
        }
        Err(error) => {
            shell.pending_upload.live = false;
            Err(error)
        }
    }
}

/// The directory the model says the pane is in.
fn pane_directory(shell: &WindowShell, key: &PaneKey) -> Option<String> {
    shell
        .hosts()
        .state()
        .model()
        .host(&key.host)?
        .model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .find(|pane| pane.id == key.pane)
        .and_then(|pane| pane.working_directory.clone())
}

/// Reads the record once, and keeps a paste that is already under way.
fn ensure_loaded(shell: &mut WindowShell) {
    if shell.pending_upload.loaded {
        return;
    }
    shell.pending_upload.loaded = true;
    let Some(path) = crate::upload_log::path_beside(shell.options.settings_path.as_deref()) else {
        return;
    };
    let records = crate::upload_log::read(&path);
    if records.is_empty() || incomplete(shell) {
        return;
    }
    shell.pending_upload.key = records.iter().find_map(|record| {
        matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting).then(|| PaneKey {
            host: HostId(record.host.clone()),
            pane: record.pane,
        })
    });
    shell.pending_upload.records = records;
}

/// Whether a file is still waiting or on its way.
fn incomplete(shell: &WindowShell) -> bool {
    shell
        .pending_upload
        .records
        .iter()
        .any(|record| matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting))
}

/// Writes the record beside the settings file, when there is one.
fn write_record(shell: &WindowShell) {
    let Some(path) = crate::upload_log::path_beside(shell.options.settings_path.as_deref()) else {
        return;
    };
    let _wrote = crate::upload_log::write(&path, &shell.pending_upload.records);
}

/// Forgets the pane a paste was aimed at. The records stay.
fn stop(shell: &mut WindowShell) {
    shell.pending_upload.key = None;
    shell.pending_upload.live = false;
}

/// Whether the host never received the file because the link was down.
fn held_for_later(detail: &str) -> bool {
    detail.starts_with("the link failed") || detail == "the host had no link to receive the file"
}

/// The length a refusal names, when the host kept a shorter temporary file.
#[must_use]
pub fn written_at(detail: &str) -> Option<u64> {
    let marker = "is at byte ";
    let start = detail.rfind(marker)?;
    let number = detail.get(start.saturating_add(marker.len())..)?.trim();
    number.parse().ok()
}

/// Points the sending file back at `at` so the next send continues there.
///
/// A file that was already rewound once is left failed: the host answered
/// with the same length again.
fn continue_from(shell: &mut WindowShell, host: &HostId, at: u64) -> bool {
    let Some(record) = shell
        .pending_upload
        .records
        .iter_mut()
        .rev()
        .find(|record| record.host == host.0 && record.phase == UploadPhase::Sending)
    else {
        return false;
    };
    if record.retry_once {
        return false;
    }
    record.retry_once = true;
    record.sent = at.min(record.total);
    record.phase = UploadPhase::Sending;
    shell.pending_upload.live = false;
    true
}

/// The host a snapshot names, so a paste can continue after the model has it.
#[must_use]
pub fn snapshot_host(event: &EngineEvent) -> Option<HostId> {
    match event {
        EngineEvent::Said(ManagerEvent::Snapshot { host, .. }) => Some(host.clone()),
        _ => None,
    }
}

/// Continues a paste after `host`'s snapshot has been applied.
pub fn resume_after(
    shell: &mut WindowShell,
    host: Option<&HostId>,
    context: &mut Context<'_, WindowShell>,
) {
    let Some(host) = host else {
        return;
    };
    ensure_loaded(shell);
    resume(shell, host, context);
}

/// Continues a paste the record still holds, once `host` is connected and the
/// pane is in the model.
fn resume(shell: &mut WindowShell, host: &HostId, context: &mut Context<'_, WindowShell>) {
    if shell.pending_upload.live {
        return;
    }
    let Some(key) = shell.pending_upload.key.clone() else {
        return;
    };
    if key.host != *host || !incomplete(shell) {
        return;
    }
    if !shell.hosts().state().accepts_upload(host) {
        return;
    }
    if pane_directory(shell, &key).is_none() && !pane_present(shell, &key) {
        fail_records(shell, host, "the pane is no longer there");
        stop(shell);
        write_record(shell);
        open_once(shell, context);
        return;
    }
    open_once(shell, context);
    if let Err(error) = send_next(shell) {
        shell.pending_upload.live = false;
        if matches!(error, crate::bridge::EngineError::Stopped) {
            fail_records(shell, host, &error.to_string());
            stop(shell);
            shell.failure(host, error.to_string(), context);
        }
    }
    write_record(shell);
}

/// Whether the model holds `key`'s pane. A directory is not required: a pane
/// that has not reported one still receives a file in the daemon's directory.
fn pane_present(shell: &WindowShell, key: &PaneKey) -> bool {
    shell
        .hosts()
        .state()
        .model()
        .host(&key.host)
        .is_some_and(|view| {
            view.model
                .sessions
                .iter()
                .flat_map(|session| session.tabs.iter())
                .flat_map(|tab| tab.panes.iter())
                .any(|pane| pane.id == key.pane)
        })
}
