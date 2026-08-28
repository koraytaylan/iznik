//! The one place that decides what a subscription starts with: contiguous
//! bytes from the ring, or the screen as truth, as a pure function over what
//! the ring holds.
//!
//! **`Screen` is the only resynchronization mechanism.** There is no marker
//! for a client to interpret and no hole for it to reason about: whenever the
//! server cannot deliver contiguous bytes from where the client is, it
//! delivers the truth at a named sequence instead, and continues from there.
//!
//! Deciding that here rather than in the pump is what makes it testable
//! exhaustively in milliseconds. The multiplexer executes a plan and proves
//! the bytes through the oracle; this says only what the plan is.

use iznik_protocol::identity::{PaneId, Sequence};

/// What a client is asking to be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartRequest {
    /// A cold attach: the client holds nothing of this pane.
    Subscribe {
        /// The pane.
        pane: PaneId,
    },
    /// A reconnect: the client holds every byte before `from`.
    Resume {
        /// The pane.
        pane: PaneId,
        /// The first byte the client does not hold.
        from: Sequence,
    },
    /// A repaint: the client wants the truth, whatever it holds.
    ScreenRequest {
        /// The pane.
        pane: PaneId,
    },
}

impl StartRequest {
    /// The pane it names, which is how the multiplexer routes it.
    #[must_use]
    pub fn pane(&self) -> PaneId {
        match self {
            StartRequest::Subscribe { pane }
            | StartRequest::Resume { pane, .. }
            | StartRequest::ScreenRequest { pane } => *pane,
        }
    }
}

/// What the subscription starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartPlan {
    /// Bytes from here, with no screen: the client's own state is still true.
    Continue {
        /// The first byte to send, which the ring still holds.
        from: Sequence,
    },
    /// The screen as it was at this sequence, and then bytes from there.
    Screen {
        /// The sequence the screen is exact at.
        at: Sequence,
    },
}

/// What a request starts with, given what the pane's ring holds.
///
/// A cold [`StartRequest::Subscribe`] gets the truth and continues from it. A
/// [`StartRequest::Resume`] the ring still covers costs nothing — the client's
/// screen is already right and the bytes it missed are still there — and one
/// it does not cover is exactly the cold attach, because a client that cannot
/// be caught up byte by byte is a client that needs the truth. A
/// [`StartRequest::ScreenRequest`] is the truth at `newest`, and the cursor
/// moves there, so no byte is delivered twice.
///
/// A plan never names a sequence the ring does not hold: `Continue { from }`
/// always satisfies `oldest <= from <= newest`, and `Screen { at }` always has
/// `at == newest`.
#[must_use]
pub fn plan_start(request: &StartRequest, oldest: Sequence, newest: Sequence) -> StartPlan {
    match request {
        StartRequest::Resume { from, .. } if oldest <= *from && *from <= newest => {
            StartPlan::Continue { from: *from }
        }
        StartRequest::Resume { .. }
        | StartRequest::Subscribe { .. }
        | StartRequest::ScreenRequest { .. } => StartPlan::Screen { at: newest },
    }
}
