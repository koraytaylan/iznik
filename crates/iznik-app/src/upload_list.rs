//! What the uploads panel does to its list: drop pastes that are finished,
//! remove one of them, or stop one that is still being sent.
//!
//! Stopping tells the piece loop to end before the next piece, and marks
//! what was still waiting so it is not sent later. Finished files inside a
//! paste that is still going stay, because they belong to that paste.

use gpui_kit::Context;

use iznik_client::host::manager::UPLOAD_CANCEL;
use iznik_protocol::identity::PaneId;

use crate::upload::{UploadGroup, UploadPhase, UploadRecord, busy, groups, stop, write_record};
use crate::window::WindowShell;

/// The records that stay when the list is cleared: a paste still sending, whole.
#[must_use]
pub fn keeping_busy(records: &[UploadRecord]) -> Vec<UploadRecord> {
    let found = groups(records);
    records
        .iter()
        .filter(|record| {
            found
                .iter()
                .any(|group| busy(&group.records) && holds(group, record))
        })
        .cloned()
        .collect()
}

/// Marks every file of the paste still open as cancelled.
///
/// The names it returns are the ones the piece loop may already be sending.
/// A file that was only waiting is marked failed and is not named, so a later
/// paste of that name is not stopped by a list it never reached.
pub fn fail_open(
    records: &mut [UploadRecord],
    host: &str,
    pane: PaneId,
    name: &str,
) -> Vec<String> {
    let Some(group) = groups(records)
        .into_iter()
        .find(|group| root_matches(group, host, pane, name))
    else {
        return Vec::new();
    };
    if !busy(&group.records) {
        return Vec::new();
    }
    let mut names = Vec::new();
    for record in records.iter_mut() {
        if !holds(&group, record) {
            continue;
        }
        if matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting) {
            if record.phase == UploadPhase::Sending {
                names.push(record.name.clone());
            }
            record.phase = UploadPhase::Failed;
            record.detail = Some(UPLOAD_CANCEL.to_owned());
        }
    }
    names
}

/// Drops every paste that is not still sending.
pub fn clear_settled(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    shell.pending_upload.records = keeping_busy(&shell.pending_upload.records);
    forget_rate(shell);
    write_record(shell);
    context.notify();
}

/// Drops one finished or failed paste. A paste still sending is left to cancel.
pub fn remove_group(
    shell: &mut WindowShell,
    host: &str,
    pane: PaneId,
    name: &str,
    context: &mut Context<'_, WindowShell>,
) {
    let Some(group) = groups(&shell.pending_upload.records)
        .into_iter()
        .find(|group| root_matches(group, host, pane, name))
    else {
        return;
    };
    if busy(&group.records) {
        return;
    }
    shell
        .pending_upload
        .records
        .retain(|record| !holds(&group, record));
    forget_rate(shell);
    write_record(shell);
    context.notify();
}

/// Stops a paste. Files still waiting are not sent, and the one in flight
/// ends before its next piece.
pub fn cancel_group(
    shell: &mut WindowShell,
    host: &str,
    pane: PaneId,
    name: &str,
    context: &mut Context<'_, WindowShell>,
) {
    let names = fail_open(&mut shell.pending_upload.records, host, pane, name);
    for stopped in &names {
        let _ignored = shell.hosts().bridge().cancel_upload(host, pane, stopped);
    }
    if !shell
        .pending_upload
        .records
        .iter()
        .any(|record| matches!(record.phase, UploadPhase::Sending | UploadPhase::Waiting))
    {
        stop(shell);
    }
    write_record(shell);
    context.notify();
}

/// Drops rate samples whose paste is no longer listed.
fn forget_rate(shell: &mut WindowShell) {
    let records = shell.pending_upload.records.clone();
    shell.pending_upload.rate.retain(|track| {
        records.iter().any(|record| {
            record.host == track.host && record.pane == track.pane && record.name == track.name
        })
    });
}

/// Whether `record` is one of `group`.
fn holds(group: &UploadGroup, record: &UploadRecord) -> bool {
    group.records.iter().any(|held| {
        held.host == record.host
            && held.pane == record.pane
            && held.name == record.name
            && held.local == record.local
    })
}

/// Whether `name` is the paste the person sees as this row.
fn root_matches(group: &UploadGroup, host: &str, pane: PaneId, name: &str) -> bool {
    group
        .records
        .iter()
        .find(|record| record.type_path)
        .or(group.records.first())
        .is_some_and(|record| record.host == host && record.pane == pane && record.name == name)
}
