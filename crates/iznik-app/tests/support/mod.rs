//! Shared bounded VT-thread fixtures for application integration tests and budgets.
use iznik_app::bridge::EngineBridge;
use iznik_app::vt::{PaneKey, TerminalSnapshot, TerminalTheme, VtEvent, VtOutput, VtThread};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::{PaneId, Sequence};
use std::time::{Duration, Instant};

/// A broken thread fails in two seconds rather than hanging the suite.
const REPLY_DEADLINE: Duration = Duration::from_secs(2);
/// Yield to the emulator thread without spinning a whole CPU.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// The isolated synthetic host used for emulator-only tests.
pub(crate) fn key() -> PaneKey {
    PaneKey {
        host: HostId("fixture".to_owned()),
        pane: PaneId(1),
    }
}

/// Await a thread reply under a short deadline.
///
/// # Panics
/// Fails the test if the owning thread never replies.
pub(crate) fn receive(thread: &VtThread) -> VtEvent {
    let started = Instant::now();
    loop {
        if let Some(event) = thread.poll() {
            return event;
        }
        assert!(started.elapsed() < REPLY_DEADLINE, "VT reply deadline");
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Extract a successful snapshot reply.
///
/// # Errors
/// Returns the emulator failure or a missing snapshot.
///
/// # Panics
/// Fails if the thread misses the reply deadline.
pub(crate) fn snapshot(thread: &VtThread) -> Result<TerminalSnapshot, Box<dyn std::error::Error>> {
    match receive(thread).result? {
        Some(VtOutput::Snapshot(snapshot)) => Ok(*snapshot),
        _ => Err("missing snapshot".into()),
    }
}

/// Open a blank pane at a caller-chosen sequence and dimensions.
///
/// # Errors
/// Returns thread or emulator failures.
///
/// # Panics
/// Fails if the thread misses the reply deadline.
pub(crate) fn open(
    thread: &VtThread,
    sequence: Sequence,
    columns: u16,
    rows: u16,
) -> Result<TerminalSnapshot, Box<dyn std::error::Error>> {
    EngineBridge::feed_terminal(
        thread,
        &ManagerEvent::Screen {
            host: key().host,
            pane: key().pane,
            sequence,
            columns,
            rows,
            bytes: Vec::new(),
        },
        &TerminalTheme::default(),
    )?;
    snapshot(thread)
}
