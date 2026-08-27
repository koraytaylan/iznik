//! The ring itself: append, range and copy by absolute sequence, without
//! allocating per chunk in steady state.
//!
//! A pane's bytes are held in a `VecDeque` bounded to the ring's capacity: it
//! grows to the capacity as bytes arrive — an idle pane costs nothing — and once
//! full it overwrites the oldest, so appends allocate a number of times that
//! depends on the capacity, never on the bytes. Every byte carries an absolute
//! sequence from the pane's creation, so a reconnecting client names the byte it
//! holds and is given exactly the bytes it missed, or told they have aged out.

use std::collections::VecDeque;

use iznik_protocol::identity::Sequence;

/// A pane's recent output, indexed by absolute sequence and bounded to a
/// capacity in bytes.
#[derive(Clone, Debug)]
pub struct PaneHistory {
    /// The bytes held, oldest at the front.
    bytes: VecDeque<u8>,
    /// The most bytes the ring holds.
    capacity: usize,
    /// Every byte ever appended, so sequences are absolute; also the sequence
    /// just past the newest byte.
    total: u64,
}

impl PaneHistory {
    /// A ring holding at most `capacity` bytes.
    #[must_use]
    pub fn new(capacity: usize) -> PaneHistory {
        PaneHistory {
            bytes: VecDeque::new(),
            capacity,
            total: 0,
        }
    }

    /// The sequence just past the newest byte — the next byte's sequence.
    #[must_use]
    pub fn newest(&self) -> Sequence {
        Sequence(self.total)
    }

    /// The sequence of the oldest byte still held.
    #[must_use]
    pub fn oldest(&self) -> Sequence {
        Sequence(self.total.saturating_sub(self.held()))
    }

    /// The capacity the ring is bounded to.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The bytes held, as a `u64`.
    fn held(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    /// Appends bytes and returns the sequence of the first one appended.
    pub fn append(&mut self, bytes: &[u8]) -> Sequence {
        let first = Sequence(self.total);
        self.total = self
            .total
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.capacity == 0 {
            return first;
        }
        // Only the last `capacity` bytes of a larger append can survive.
        let effective = if bytes.len() > self.capacity {
            bytes
                .get(bytes.len().saturating_sub(self.capacity)..)
                .unwrap_or(bytes)
        } else {
            bytes
        };
        self.bytes.extend(effective);
        self.trim();
        first
    }

    /// Drops the oldest bytes until the ring is within its capacity.
    fn trim(&mut self) {
        while self.bytes.len() > self.capacity {
            let excess = self.bytes.len().saturating_sub(self.capacity);
            let _dropped = self.bytes.drain(..excess);
        }
    }

    /// Sets a new capacity, dropping the oldest bytes over it — how the budget
    /// shrinks a ring.
    pub(crate) fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        self.trim();
    }

    /// The bytes from `from` to the newest, as at most two contiguous slices.
    ///
    /// # Errors
    ///
    /// [`HistoryError::AgedOut`] when `from` is older than the oldest byte held.
    pub fn range(&self, from: Sequence) -> Result<HistorySlices<'_>, HistoryError> {
        let oldest = self.total.saturating_sub(self.held());
        if from.0 < oldest {
            return Err(HistoryError::AgedOut {
                oldest: Sequence(oldest),
            });
        }
        let clamped = from.0.min(self.total);
        let offset = usize::try_from(clamped.saturating_sub(oldest)).unwrap_or(self.bytes.len());
        let (front, back) = self.bytes.as_slices();
        Ok(slices_from(front, back, offset))
    }

    /// Appends up to `maximum` bytes from `from` into `into`, and returns the
    /// sequence just past what it copied.
    ///
    /// # Errors
    ///
    /// [`HistoryError::AgedOut`] when `from` is older than the oldest byte held.
    pub fn copy_range(
        &self,
        from: Sequence,
        maximum: usize,
        into: &mut Vec<u8>,
    ) -> Result<Sequence, HistoryError> {
        let slices = self.range(from)?;
        let mut copied = 0_usize;
        for slice in [slices.first, slices.second] {
            if copied >= maximum {
                break;
            }
            let take = slice.len().min(maximum.saturating_sub(copied));
            if let Some(piece) = slice.get(..take) {
                into.extend_from_slice(piece);
                copied = copied.saturating_add(take);
            }
        }
        Ok(Sequence(
            from.0
                .saturating_add(u64::try_from(copied).unwrap_or(u64::MAX)),
        ))
    }
}

/// The two slices `from` a wrapped ring: the second is empty when the range is
/// contiguous.
#[derive(Clone, Copy, Debug)]
pub struct HistorySlices<'history> {
    /// The first, and possibly only, slice.
    pub first: &'history [u8],
    /// The second slice, empty when the range did not wrap.
    pub second: &'history [u8],
}

impl HistorySlices<'_> {
    /// The total number of bytes across both slices.
    #[must_use]
    pub fn len(&self) -> usize {
        self.first.len().saturating_add(self.second.len())
    }

    /// Whether the range is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.first.is_empty() && self.second.is_empty()
    }
}

/// The slices of `front` then `back` from `offset` on, dropping the first
/// `offset` bytes.
fn slices_from<'history>(
    front: &'history [u8],
    back: &'history [u8],
    offset: usize,
) -> HistorySlices<'history> {
    if offset < front.len() {
        HistorySlices {
            first: front.get(offset..).unwrap_or_default(),
            second: back,
        }
    } else {
        HistorySlices {
            first: back
                .get(offset.saturating_sub(front.len())..)
                .unwrap_or_default(),
            second: &[],
        }
    }
}

/// Why a range could not be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    /// `from` is older than the oldest byte the ring still holds.
    AgedOut {
        /// The oldest byte still held.
        oldest: Sequence,
    },
}

impl core::fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HistoryError::AgedOut { oldest } => {
                write!(
                    formatter,
                    "the bytes have aged out; the oldest held is {oldest:?}"
                )
            }
        }
    }
}

impl std::error::Error for HistoryError {}
