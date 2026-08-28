//! The channel table that hands out pane channels and holds a released one
//! until the client acknowledges it, and the per-channel cursor.
//!
//! A frame on channel `n` is pane output and is never parsed, so the only
//! thing that says which pane it belongs to is the channel number. That is why
//! a released channel is not free at once: a frame already on its way when the
//! pane was detached would otherwise be delivered to whatever pane was given
//! the number next, and painted into the wrong surface. The client's
//! `ChannelReleased` is what says the wire is clear.

use std::collections::{BTreeMap, BTreeSet};

use core::fmt::{self, Display, Formatter};

use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::MessageError;

use crate::multiplexer::credit::CreditWindow;

/// The lowest channel pane output may flow on; channel 0 carries control.
const FIRST_PANE_CHANNEL: u8 = 1;

/// Why a frame could not be handed to the link under the multiplexer.
///
/// The in-memory sink of plan 0003 never fails; plan 0004 puts a framed link
/// beneath it and maps what the link says onto these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkError {
    /// The link is gone: the client disconnected, or its half was closed.
    /// Nothing more can be sent, and the multiplexer stops rather than
    /// buffering for a client that is not there.
    Closed,
    /// The link could not take the frame, and said why.
    Refused {
        /// What it said.
        detail: String,
    },
}

impl Display for SinkError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SinkError::Closed => write!(formatter, "the link is closed"),
            SinkError::Refused { detail } => {
                write!(formatter, "the link refused a frame: {detail}")
            }
        }
    }
}

impl core::error::Error for SinkError {}

/// Why the multiplexer refused something.
///
/// The connection loop maps each of these to the protocol's `ErrorCode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiplexerError {
    /// All 255 pane channels are assigned or await acknowledgement.
    ChannelsExhausted,
    /// The host holds no such pane.
    UnknownPane {
        /// The pane that was named.
        pane: PaneId,
    },
    /// The client acted on a pane it is not subscribed to.
    NotSubscribed {
        /// The pane that was named.
        pane: PaneId,
    },
    /// The client acknowledged a channel that was not waiting to be released,
    /// which means the two ends disagree about what is on the wire.
    NotReleased {
        /// The channel that was acknowledged.
        channel: u8,
    },
    /// A frame could not be built: a model or a delta larger than a frame
    /// carries. Nothing was sent, and the client is told rather than left
    /// waiting for something that cannot be encoded.
    Encoding {
        /// What the codec said.
        detail: String,
    },
    /// The link under the multiplexer could not take a frame.
    Sink(SinkError),
}

impl Display for MultiplexerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            MultiplexerError::ChannelsExhausted => {
                write!(formatter, "every pane channel is spoken for")
            }
            MultiplexerError::UnknownPane { pane } => {
                write!(formatter, "the host holds no pane {}", pane.0)
            }
            MultiplexerError::NotSubscribed { pane } => {
                write!(
                    formatter,
                    "this client is not subscribed to pane {}",
                    pane.0
                )
            }
            MultiplexerError::NotReleased { channel } => write!(
                formatter,
                "channel {channel} was not waiting to be released"
            ),
            MultiplexerError::Encoding { detail } => {
                write!(formatter, "a frame could not be built: {detail}")
            }
            MultiplexerError::Sink(error) => write!(formatter, "{error}"),
        }
    }
}

impl core::error::Error for MultiplexerError {}

impl From<MessageError> for MultiplexerError {
    fn from(error: MessageError) -> MultiplexerError {
        MultiplexerError::Encoding {
            detail: error.to_string(),
        }
    }
}

impl From<SinkError> for MultiplexerError {
    fn from(error: SinkError) -> MultiplexerError {
        MultiplexerError::Sink(error)
    }
}

/// Which pane each channel carries, and which channels are not free yet.
#[derive(Debug, Default)]
pub struct ChannelTable {
    /// The pane each assigned channel carries.
    panes: BTreeMap<u8, PaneId>,
    /// The channel each subscribed pane's output flows on.
    channels: BTreeMap<PaneId, u8>,
    /// Channels released but not yet acknowledged by the client. A frame may
    /// still be in flight on each, so none may be handed out again.
    released_pending: BTreeSet<u8>,
}

impl ChannelTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> ChannelTable {
        ChannelTable::default()
    }

    /// Hands the pane the lowest channel that is neither assigned nor waiting
    /// to be released, or the one it already has.
    ///
    /// A pane can be asked for twice — a repaint and a resume are both a
    /// subscription — and handing out a second channel for it would leave the
    /// table disagreeing with itself and the first channel unreachable for the
    /// life of the connection, since nothing but the pane's own entry can
    /// release it.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::ChannelsExhausted`] when none of the 255 is free.
    pub fn assign(&mut self, pane: PaneId) -> Result<u8, MultiplexerError> {
        if let Some(held) = self.channel_of(pane) {
            return Ok(held);
        }
        let channel = (FIRST_PANE_CHANNEL..=u8::MAX)
            .find(|candidate| {
                !self.panes.contains_key(candidate) && !self.released_pending.contains(candidate)
            })
            .ok_or(MultiplexerError::ChannelsExhausted)?;
        let _carried = self.panes.insert(channel, pane);
        let _flowed = self.channels.insert(pane, channel);
        Ok(channel)
    }

    /// Stops carrying a channel's pane and holds the number back until the
    /// client acknowledges it. A channel nothing is assigned to is left alone:
    /// there is nothing in flight to hold back.
    pub fn release(&mut self, channel: u8) {
        if let Some(pane) = self.panes.remove(&channel) {
            // Only the pane's own entry, so that a table which somehow held
            // two channels for one pane does not lose the other's reverse
            // mapping and leak it.
            if self.channels.get(&pane) == Some(&channel) {
                let _released = self.channels.remove(&pane);
            }
            let _added = self.released_pending.insert(channel);
        }
    }

    /// Returns a released channel to the free set: the client has said nothing
    /// of the old pane is still on the wire.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::NotReleased`] naming a channel that was not waiting
    /// to be released, which means the two ends disagree about the wire.
    pub fn acknowledge(&mut self, channel: u8) -> Result<(), MultiplexerError> {
        if self.released_pending.remove(&channel) {
            Ok(())
        } else {
            Err(MultiplexerError::NotReleased { channel })
        }
    }

    /// The channel a pane's output flows on, while it flows.
    #[must_use]
    pub fn channel_of(&self, pane: PaneId) -> Option<u8> {
        self.channels.get(&pane).copied()
    }

    /// The pane a channel carries, while it carries one.
    #[must_use]
    pub fn pane_of(&self, channel: u8) -> Option<PaneId> {
        self.panes.get(&channel).copied()
    }

    /// How many channels are assigned.
    #[must_use]
    pub fn assigned(&self) -> usize {
        self.panes.len()
    }

    /// How many channels are waiting to be acknowledged.
    #[must_use]
    pub fn awaiting_acknowledgement(&self) -> usize {
        self.released_pending.len()
    }
}

/// Where one subscription has got to, and how much further it may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The pane it reads.
    pub pane: PaneId,
    /// The channel its bytes go out on.
    pub channel: u8,
    /// The next byte to send.
    pub sequence: Sequence,
    /// How much further it may go before the client refills.
    pub credit: CreditWindow,
    /// Whether it has fallen so far behind that the truth is cheaper than the
    /// bytes, so it is served a screen when it is looked at again.
    pub stale: bool,
}

impl Cursor {
    /// A cursor at a sequence, with the window a pane nobody is looking at
    /// gets.
    #[must_use]
    pub fn new(pane: PaneId, channel: u8, sequence: Sequence) -> Cursor {
        Cursor {
            pane,
            channel,
            sequence,
            credit: CreditWindow::background(),
            stale: false,
        }
    }

    /// How far behind the pane's newest byte it is. A cursor ahead of the
    /// pane — which nothing produces — is not behind at all.
    #[must_use]
    pub fn lag(&self, newest: Sequence) -> u64 {
        newest.0.saturating_sub(self.sequence.0)
    }
}
