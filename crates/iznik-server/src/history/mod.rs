//! Per-pane history rings indexed by absolute sequence, and the shared budget that evicts from the least recently focused pane first.
//!
//! Filled by task `history-ring` of plan 0002; until then this module holds only its documentation.

pub mod ring;
