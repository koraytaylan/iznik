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

/// How many bytes may still be sent on one channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CreditWindow {
    /// The bytes the client has said it has room for and has not been sent.
    available: u32,
}

impl CreditWindow {
    /// The window a pane nobody is looking at starts with.
    #[must_use]
    pub fn background() -> CreditWindow {
        CreditWindow {
            available: INITIAL_CREDIT_BYTES,
        }
    }

    /// The window the pane a client is looking at starts with.
    #[must_use]
    pub fn focused() -> CreditWindow {
        CreditWindow {
            available: FOCUSED_CREDIT_BYTES,
        }
    }

    /// The bytes that may still be sent.
    #[must_use]
    pub fn available(self) -> u32 {
        self.available
    }

    /// Takes bytes from the window and says how many it took, which is never
    /// more than were there: a scheduler asks for what it has to send and
    /// sends what the window allows.
    pub fn consume(&mut self, bytes: u32) -> u32 {
        let taken = bytes.min(self.available);
        self.available = self.available.saturating_sub(taken);
        taken
    }

    /// Gives bytes back, saturating rather than overflowing: a client that
    /// refills more than it ever consumed is confused, not an arithmetic
    /// fault, and the window is a ceiling either way.
    pub fn refill(&mut self, bytes: u32) {
        self.available = self.available.saturating_add(bytes);
    }

    /// Widens the window to the focused size, which is what focus moving here
    /// means. It never narrows: credit a client has already granted is the
    /// client's promise, and this only tops it up to the size the contract
    /// says a focused pane may use.
    pub fn widen(&mut self) {
        self.available = self.available.max(FOCUSED_CREDIT_BYTES);
    }

    /// Narrows the window to the background size, which is what focus leaving
    /// means. Sending less than a client offered is always safe.
    pub fn narrow(&mut self) {
        self.available = self.available.min(INITIAL_CREDIT_BYTES);
    }
}
