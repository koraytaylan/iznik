//! Exact byte budgets and markers for per-pane surface backpressure.

/// The protocol's initial unfocused credit window is a quarter mebibyte.
pub(crate) const CREDIT_BYTES: usize = 256 * 1024;
/// Twice the initial credit requires returning consumed delivery receipts to finish.
/// This is below the four-mebibyte history ring, so replay does not replace the test.
pub(crate) const FLOOD_BYTES: usize = 512 * 1024;
/// A small terminal keeps the test about flow control rather than rendering load.
pub(crate) const COLUMNS: u16 = 40;
/// Enough rows to hold both healthy probes and the completed flood marker.
pub(crate) const ROWS: u16 = 4;
/// Native output confirms that echo is disabled before any flood bytes are sent.
pub(crate) const READY: &str = "surface-reader-ready";
/// The flood's final bytes cannot arrive until consumed snapshots return credit.
pub(crate) const FINISHED: &str = "surface-flood-finished";
/// First healthy response while the sibling's surface is not consuming.
pub(crate) const FIRST: &str = "healthy-first";
/// A second round trip proves the exhausted pane stays bounded across live traffic.
pub(crate) const SECOND: &str = "healthy-second";
