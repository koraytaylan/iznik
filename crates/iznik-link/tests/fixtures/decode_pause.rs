//! Idle polls must wait for compressed input without exhausting the codec's progress guard.

/// More idle polls than zstd's empty-input progress guard allows, without new peer bytes.
pub(crate) const IDLE_POLLS: usize = 32;
/// First delivery establishes a decoder that has consumed real compressed input.
pub(crate) const BEFORE_IDLE: &[u8] = b"output before idle polling";
/// Bytes written after the idle polls must still decode completely and in order.
pub(crate) const AFTER_IDLE: &[u8] = b"output after idle polling";
