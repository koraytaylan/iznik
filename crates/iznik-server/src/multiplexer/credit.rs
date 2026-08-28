//! Credit windows in bytes: the focused pane's larger window, refills,
//! consumption, and the stale threshold.
//!
//! A window is how many bytes the server may send on a channel before the
//! client says it has room for more. The client refills as its surface
//! consumes, so the server can never outrun what the client can hold, and a
//! pane nobody is reading cannot fill memory at either end.
//!
//! The sizes are a contract both ends know: a pane a client is showing gets
//! [`FOCUSED_CREDIT_BYTES`], one it is not gets [`INITIAL_CREDIT_BYTES`], and
//! moving focus moves the larger window with it.

/// The window a pane nobody is looking at starts with, and the ceiling its
/// window returns to when focus leaves it. A quarter of a megabyte is several
/// screens of output — enough that a background pane keeps up with ordinary
/// work — and small enough that a hundred of them cannot cost a client more
/// than it can hold.
pub const INITIAL_CREDIT_BYTES: u32 = 256 * 1024;

/// The window the pane a client is looking at gets. Four times the background
/// window, because it is the one whose latency a person can see.
pub const FOCUSED_CREDIT_BYTES: u32 = 1024 * 1024;

/// The most one pane frame carries, so a keystroke echo waits behind at most
/// one frame per active pane rather than behind a whole window.
pub const FRAME_PAYLOAD_LENGTH: u32 = 64 * 1024;

/// The lag past which a background cursor stops being served and is marked
/// stale. Beyond this the client is so far behind that catching it up byte by
/// byte costs more than sending it the truth, which is what happens when it is
/// looked at again. It is at least [`FOCUSED_CREDIT_BYTES`], so a cursor
/// cannot be marked stale while its own window would still have carried it.
pub const STALE_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;

/// How much more the focused window holds than a background one, which is
/// what focus moving adds to a window and focus leaving takes away.
const FOCUS_INCREMENT: u32 = FOCUSED_CREDIT_BYTES.saturating_sub(INITIAL_CREDIT_BYTES);

/// How many bytes may still be sent on one channel.
///
/// `available` is what is left of the client's grant — what it has said it can
/// hold, less what has gone out — and `ceiling` is how much it can hold at
/// all. Keeping both is what makes focus safe: moving the larger window to a
/// pane adds the difference between the two sizes rather than setting the
/// remainder to the larger one, so the bytes already in flight to the client
/// still count against what it can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CreditWindow {
    /// The bytes the client has room for that have not been sent.
    available: u32,
    /// The most the client can hold for this pane at once.
    ceiling: u32,
}

impl CreditWindow {
    /// The window a pane nobody is looking at starts with.
    #[must_use]
    pub fn background() -> CreditWindow {
        CreditWindow {
            available: INITIAL_CREDIT_BYTES,
            ceiling: INITIAL_CREDIT_BYTES,
        }
    }

    /// The window the pane a client is looking at starts with.
    #[must_use]
    pub fn focused() -> CreditWindow {
        CreditWindow {
            available: FOCUSED_CREDIT_BYTES,
            ceiling: FOCUSED_CREDIT_BYTES,
        }
    }

    /// The bytes that may still be sent.
    #[must_use]
    pub fn available(self) -> u32 {
        self.available
    }

    /// The most the client can hold for this pane at once.
    #[must_use]
    pub fn ceiling(self) -> u32 {
        self.ceiling
    }

    /// Takes bytes from the window and says how many it took, which is never
    /// more than were there: a scheduler asks for what it has to send and
    /// sends what the window allows.
    pub fn consume(&mut self, bytes: u32) -> u32 {
        let taken = bytes.min(self.available);
        self.available = self.available.saturating_sub(taken);
        taken
    }

    /// Gives bytes back as the client consumes them, never past the ceiling:
    /// a client that returns more credit than it was ever sent is confused,
    /// and letting the window grow on its word would let one pane fill its
    /// memory.
    pub fn refill(&mut self, bytes: u32) {
        self.available = self.available.saturating_add(bytes).min(self.ceiling);
    }

    /// Moves the larger window here, which is what focus arriving means. It
    /// adds the difference between the two sizes rather than setting the
    /// remainder to the larger one, so bytes already in flight still count
    /// against what the client can hold — and it never takes credit back.
    pub fn widen(&mut self) {
        self.ceiling = FOCUSED_CREDIT_BYTES;
        self.available = self
            .available
            .saturating_add(FOCUS_INCREMENT)
            .min(self.ceiling);
    }

    /// Takes the larger window away again, which is what focus leaving means.
    /// It subtracts the same difference, so what is outstanding at the client
    /// plus what may still be sent stays inside the background ceiling.
    pub fn narrow(&mut self) {
        self.ceiling = INITIAL_CREDIT_BYTES;
        self.available = self
            .available
            .saturating_sub(FOCUS_INCREMENT)
            .min(self.ceiling);
    }
}
