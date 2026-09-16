//! A producer held until the UI disconnects, then beyond the server's history ring.

/// More output than the default four-mebibyte pane ring can retain.
/// Five mebibytes leaves a whole mebibyte margin beyond the held resume cursor.
pub(crate) const FLOOD_BYTES: u64 = 5 * 1024 * 1024;
/// Printed before waiting for the test to disconnect and release the producer.
pub(crate) const READY: &str = "cold-producer-armed";
/// Printed after the flood and retained at the bottom of the authoritative screen.
pub(crate) const FINISHED: &str = "cold-producer-finished";
/// The isolated host's release file; the fixture owns the whole container.
pub(crate) const RELEASE: &str = "/tmp/window-cold-release";
/// The isolated host's completion file avoids guessing when the producer finished.
pub(crate) const COMPLETE: &str = "/tmp/window-cold-complete";
