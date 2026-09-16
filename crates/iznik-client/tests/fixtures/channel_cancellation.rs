//! Frames from one peer write; cancellation must not remove the middle delivery.

/// Ordinary pane channel, distinct from handshake control traffic.
pub(crate) const CHANNEL: u8 = 7;
/// Prime the decoder, leaving subsequent complete frames buffered.
pub(crate) const FIRST: &[u8] = b"first";
/// The delivery polled while only one cooperative operation remains.
pub(crate) const SECOND: &[u8] = b"second";
/// A sentinel that makes a discarded second delivery immediately observable.
pub(crate) const THIRD: &[u8] = b"third";
/// The complete expected pane sequence, including the cancellation boundary.
pub(crate) const EXPECTED: &[&[u8]] = &[FIRST, SECOND, THIRD];
