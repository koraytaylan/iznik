//! One scheduling round: the focused cursor first, then round-robin, a cursor
//! at zero credit skipped, a lagging background cursor marked stale.
//!
//! The decisions are here, as pure functions over cursors and sequence
//! numbers, so that what the pump does with a link and a pane is separable
//! from what it decides — and so the fairness and priority rules can be read
//! in one place rather than inferred from a loop.

use iznik_protocol::identity::{PaneId, Sequence};

use crate::multiplexer::channel::Cursor;
use crate::multiplexer::credit::{FRAME_PAYLOAD_LENGTH, STALE_THRESHOLD_BYTES};

/// The order one round serves cursors in: the focused one first, then every
/// other from where the last round left off.
///
/// Starting the rest at a moving place is what makes them fair. Serving them
/// in a fixed order would let the first of three flooding panes take its whole
/// window every round and leave the third whatever is left.
#[must_use]
pub fn order(cursors: &[PaneId], focused: Option<PaneId>, turn: usize) -> Vec<PaneId> {
    let rest: Vec<PaneId> = cursors
        .iter()
        .copied()
        .filter(|pane| Some(*pane) != focused)
        .collect();
    let mut served = Vec::with_capacity(cursors.len());
    if let Some(pane) = focused
        && cursors.contains(&pane)
    {
        served.push(pane);
    }
    if !rest.is_empty() {
        let start = turn.checked_rem(rest.len()).unwrap_or(0);
        served.extend(rest.get(start..).unwrap_or_default());
        served.extend(rest.get(..start).unwrap_or_default());
    }
    served
}

/// How many bytes a cursor may be sent this turn: what its window allows, but
/// never more than one frame carries, so a keystroke echo waits behind at most
/// one frame per active pane rather than behind a whole window.
#[must_use]
pub fn allowance(cursor: &Cursor) -> usize {
    let bytes = cursor.credit.available().min(FRAME_PAYLOAD_LENGTH);
    usize::try_from(bytes).unwrap_or(0)
}

/// Whether a cursor has fallen so far behind that catching it up byte by byte
/// costs more than sending it the truth.
///
/// The focused cursor is never stale: it is the one a person is looking at,
/// and it is served first and with the larger window, so falling this far
/// behind while focused means the client is not reading at all — and a screen
/// would not help it.
#[must_use]
pub fn has_fallen_behind(cursor: &Cursor, newest: Sequence, focused: bool) -> bool {
    !focused && cursor.lag(newest) > STALE_THRESHOLD_BYTES
}
