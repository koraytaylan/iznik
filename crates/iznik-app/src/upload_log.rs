//! The uploads a window had not finished, written so the next launch continues them.
//!
//! One line per file. Fields are separated by a tab, and a tab, a backslash
//! or a new line inside a field is escaped, so a path may contain spaces.
//! A line that does not parse is skipped. The file is replaced atomically,
//! so a launch never reads a half-written record. Finished files stay in the
//! list; only a file that was still waiting or sending is sent again.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use iznik_protocol::identity::PaneId;

/// The file name of the record, beside the settings file.
const LOG_FILE: &str = "upload-log";

/// The mark every record line starts with.
const FILE_MARK: &str = "file";

/// Field separator.
const SEPARATOR: char = '\t';

/// A phase as the file writes it.
const WAITING: &str = "waiting";
/// A phase as the file writes it.
const SENDING: &str = "sending";
/// A phase as the file writes it.
const FINISHED: &str = "finished";
/// A phase as the file writes it.
const FAILED: &str = "failed";

/// Where a pasted file is in being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadPhase {
    /// Queued behind a file that is still sending.
    Waiting,
    /// Pieces are on their way.
    Sending,
    /// The host has the file.
    Finished,
    /// The host does not have the file.
    Failed,
}

/// One pasted file, as the uploads panel draws it and the record stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRecord {
    /// The name relative to the pane's directory.
    pub name: String,
    /// The host it was sent to.
    pub host: String,
    /// The pane whose directory receives it.
    pub pane: PaneId,
    /// The path on this machine.
    pub local: PathBuf,
    /// Bytes sent.
    pub sent: u64,
    /// Bytes the file holds. A directory is zero.
    pub total: u64,
    /// The path on the host, once the file is there.
    pub remote: Option<String>,
    /// Why it was not written, when it was not.
    pub detail: Option<String>,
    /// Where it is in being sent.
    pub phase: UploadPhase,
    /// Whether the host's path is typed into the pane.
    pub type_path: bool,
    /// Whether this creates a directory.
    pub directory: bool,
    /// Whether a resume already rewound this file once.
    ///
    /// A second rewind would send the file again forever when the host keeps
    /// answering with the same length.
    pub retry_once: bool,
}

/// The record beside `settings`, or `None` when that file has no directory.
#[must_use]
pub fn path_beside(settings: Option<&Path>) -> Option<PathBuf> {
    let parent = settings?
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())?;
    Some(parent.join(LOG_FILE))
}

/// Read a record. A missing or unreadable file is an empty list.
#[must_use]
pub fn read(path: &Path) -> Vec<UploadRecord> {
    std::fs::read_to_string(path).map_or_else(|_| Vec::new(), |text| decode(&text))
}

/// Replace the record at `path`.
///
/// # Errors
///
/// Returns the operating system's error when the directory cannot be created
/// or the file cannot be replaced.
pub fn write(path: &Path, records: &[UploadRecord]) -> Result<(), std::io::Error> {
    crate::configuration_file::replace(path, encode(records).as_bytes())
}

/// The record as the file holds it.
#[must_use]
pub fn encode(records: &[UploadRecord]) -> String {
    let mut text = String::new();
    for record in records {
        let _written = writeln!(text, "{}", line(record));
    }
    text
}

/// The records a file's text holds. A line that does not parse is skipped.
#[must_use]
pub fn decode(text: &str) -> Vec<UploadRecord> {
    text.lines().filter_map(parse_line).collect()
}

/// One record line.
fn line(record: &UploadRecord) -> String {
    let remote = record.remote.clone().unwrap_or_default();
    let detail = record.detail.clone().unwrap_or_default();
    [
        FILE_MARK,
        phase_text(record.phase),
        &record.pane.0.to_string(),
        &record.sent.to_string(),
        &record.total.to_string(),
        flag(record.type_path),
        flag(record.directory),
        flag(record.retry_once),
        &escape(&record.host),
        &escape(&record.local.display().to_string()),
        &escape(&record.name),
        &escape(&remote),
        &escape(&detail),
    ]
    .join(&SEPARATOR.to_string())
}

/// A line parsed, or nothing when it is not a record.
fn parse_line(line: &str) -> Option<UploadRecord> {
    let mut fields = line.split(SEPARATOR);
    if fields.next()? != FILE_MARK {
        return None;
    }
    let phase = phase(fields.next()?)?;
    let pane = PaneId(fields.next()?.parse().ok()?);
    let sent = fields.next()?.parse().ok()?;
    let total = fields.next()?.parse().ok()?;
    let type_path = flag_value(fields.next()?)?;
    let directory = flag_value(fields.next()?)?;
    let retry_once = flag_value(fields.next()?)?;
    let host = decode_field(fields.next()?)?;
    let local = PathBuf::from(decode_field(fields.next()?)?);
    let name = decode_field(fields.next()?)?;
    let remote = optional(decode_field(fields.next()?)?);
    let detail = optional(decode_field(fields.next()?)?);
    if fields.next().is_some() || host.is_empty() || name.is_empty() || local.as_os_str().is_empty()
    {
        return None;
    }
    Some(UploadRecord {
        name,
        host,
        pane,
        local,
        sent,
        total,
        remote,
        detail,
        phase,
        type_path,
        directory,
        retry_once,
    })
}

/// `1` or `0`.
fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

/// A flag field.
fn flag_value(text: &str) -> Option<bool> {
    match text {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// An empty field is absent.
fn optional(text: String) -> Option<String> {
    if text.is_empty() { None } else { Some(text) }
}

/// The file's word for a phase.
fn phase_text(phase: UploadPhase) -> &'static str {
    match phase {
        UploadPhase::Waiting => WAITING,
        UploadPhase::Sending => SENDING,
        UploadPhase::Finished => FINISHED,
        UploadPhase::Failed => FAILED,
    }
}

/// The phase a word names.
fn phase(text: &str) -> Option<UploadPhase> {
    match text {
        WAITING => Some(UploadPhase::Waiting),
        SENDING => Some(UploadPhase::Sending),
        FINISHED => Some(UploadPhase::Finished),
        FAILED => Some(UploadPhase::Failed),
        _ => None,
    }
}

/// Escapes a tab, a backslash and a new line so a field stays one field.
fn escape(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// The field `escape` wrote, or nothing when the escapes are not paired.
fn decode_field(text: &str) -> Option<String> {
    let mut plain = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            plain.push(character);
            continue;
        }
        match characters.next()? {
            '\\' => plain.push('\\'),
            't' => plain.push('\t'),
            'n' => plain.push('\n'),
            'r' => plain.push('\r'),
            _ => return None,
        }
    }
    Some(plain)
}
