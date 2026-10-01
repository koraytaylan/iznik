//! The process log keeps a line that was logged, and drops the oldest once it is full.

use std::sync::{Mutex, MutexGuard, PoisonError};

use iznik_app::tools_log::{self, KEPT_LINES};

/// The process-wide record, so the two cases do not write it at once.
fn hold_record() -> MutexGuard<'static, ()> {
    static GATE: Mutex<()> = Mutex::new(());
    GATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A line logged after install is in the record.
///
/// # Panics
///
/// Panics when the line is absent.
#[test]
fn install_keeps_a_log_line() {
    let _held = hold_record();
    tools_log::install();
    tracing::info!("tools log fixture");
    let lines = tools_log::snapshot();
    assert!(
        lines.iter().any(|line| line.contains("tools log fixture")),
        "{lines:?}"
    );
}

/// A full record drops the oldest line and keeps the newest at the end.
///
/// # Panics
///
/// Panics when the bound or the order is wrong.
#[test]
fn log_drops_the_oldest_line() {
    let _held = hold_record();
    for index in 0..KEPT_LINES {
        tools_log::record(&format!("old-{index}"));
    }
    tools_log::record("newest-line");
    let lines = tools_log::snapshot();
    assert_eq!(lines.len(), KEPT_LINES, "the record stays bounded");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("newest-line"),
        "the newest line is last, where a console follows it"
    );
    assert!(
        !lines.iter().any(|line| line == "old-0"),
        "the oldest line was dropped"
    );
}
