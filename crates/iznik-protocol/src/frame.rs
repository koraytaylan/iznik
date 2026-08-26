//! The frame codec: a little-endian payload length, a channel byte and the
//! payload, with a decoder that resumes across any read boundary. Channel 0
//! carries control messages; channels 1 to 255 carry pane output as raw
//! bytes, which is the one performance decision the whole system rests on.
//! The golden fixture `tests/fixtures/frame.jsonl` is the contract; this code
//! is held to it.

use core::fmt::{self, Display, Formatter};

/// The most bytes a payload may carry: one mebibyte. A larger length in a
/// header is a protocol error, not an allocation.
pub const MAXIMUM_PAYLOAD_LENGTH: u32 = 1 << 20;

/// The bytes a header takes: a four-byte little-endian length and a channel.
pub const HEADER_LENGTH: usize = 5;

/// The bytes of the length field inside a header.
const LENGTH_FIELD_WIDTH: usize = 4;

/// How many consumed bytes the decoder lets accumulate at the front of its
/// buffer before moving what remains to the front: sixty-four kibibytes, so
/// the partial frame at a read boundary is moved rarely.
const COMPACTION_THRESHOLD: usize = 64 * 1024;

/// A frame's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// The payload's length in bytes.
    pub length: u32,
    /// The channel the payload travels on.
    pub channel: u8,
}

impl FrameHeader {
    /// Appends the header's five bytes.
    pub fn write(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.length.to_le_bytes());
        out.push(self.channel);
    }

    /// The header at the start of `bytes`, or `None` when fewer than
    /// [`HEADER_LENGTH`] bytes are there.
    #[must_use]
    pub fn read(bytes: &[u8]) -> Option<FrameHeader> {
        let length = bytes
            .get(..LENGTH_FIELD_WIDTH)
            .and_then(|field| <[u8; LENGTH_FIELD_WIDTH]>::try_from(field).ok())
            .map(u32::from_le_bytes)?;
        let channel = *bytes.get(LENGTH_FIELD_WIDTH)?;
        Some(FrameHeader { length, channel })
    }
}

/// A decoded frame, borrowing its payload from the decoder's buffer.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'decoder> {
    /// The channel the payload travels on.
    pub channel: u8,
    /// The payload, exactly as it was sent.
    pub payload: &'decoder [u8],
}

/// The one decode failure a length-prefixed codec has; everything else is
/// "need more bytes".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// A length over [`MAXIMUM_PAYLOAD_LENGTH`], carrying the length that was
    /// asked for; nothing was allocated for it.
    Oversize {
        /// The length the header named, or the payload's, saturated to
        /// [`u32::MAX`] when a header could not even count it.
        length: u32,
    },
}

impl Display for FrameError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Oversize { length } => write!(
                formatter,
                "a payload of {length} bytes exceeds the maximum of {MAXIMUM_PAYLOAD_LENGTH}"
            ),
        }
    }
}

impl core::error::Error for FrameError {}

/// Appends one frame — header, then payload — to `out`.
///
/// # Errors
///
/// [`FrameError::Oversize`] when the payload is longer than
/// [`MAXIMUM_PAYLOAD_LENGTH`], before anything is appended; a payload beyond
/// what a header can even count is reported as [`u32::MAX`].
pub fn encode(channel: u8, payload: &[u8], out: &mut Vec<u8>) -> Result<(), FrameError> {
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    if length > MAXIMUM_PAYLOAD_LENGTH {
        return Err(FrameError::Oversize { length });
    }
    FrameHeader { length, channel }.write(out);
    out.extend_from_slice(payload);
    Ok(())
}

/// Reassembles frames from a byte stream split wherever the transport likes.
///
/// Bytes are pushed as they arrive; [`FrameDecoder::next_frame`] yields each
/// complete frame borrowed from the buffer, so a payload is copied once — into
/// the buffer — and never again: the buffer is compacted only when the decoder
/// asks for more bytes, and what remains then is at most one partial frame,
/// which is moved to the front at most once, and only when the consumed
/// prefix before it has outgrown the compaction threshold of sixty-four
/// kibibytes.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    /// Bytes pushed and not yet compacted away.
    buffer: Vec<u8>,
    /// How many bytes at the front of the buffer belong to frames already
    /// yielded.
    consumed: usize,
}

impl FrameDecoder {
    /// An empty decoder.
    #[must_use]
    pub fn new() -> FrameDecoder {
        FrameDecoder::default()
    }

    /// Appends bytes from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The bytes the buffer can hold without growing: what a test asserts a
    /// bound on after a long stream.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// The next complete frame, or `None` until more bytes arrive.
    ///
    /// The frame yielded by the previous call is released by this one. The
    /// buffer is compacted only on the way to `None`, when nothing complete
    /// is left in it.
    ///
    /// # Errors
    ///
    /// [`FrameError::Oversize`] when the header at the front names a length
    /// over [`MAXIMUM_PAYLOAD_LENGTH`]; the stream is corrupt from there, the
    /// decoder keeps returning the error, and the caller drops the link.
    pub fn next_frame(&mut self) -> Result<Option<Frame<'_>>, FrameError> {
        let pending = self.buffer.get(self.consumed..).unwrap_or_default();
        let Some(header) = FrameHeader::read(pending) else {
            self.compact();
            return Ok(None);
        };
        if header.length > MAXIMUM_PAYLOAD_LENGTH {
            return Err(FrameError::Oversize {
                length: header.length,
            });
        }
        let length = usize::try_from(header.length).unwrap_or(usize::MAX);
        let start = self.consumed.saturating_add(HEADER_LENGTH);
        let end = start.saturating_add(length);
        if end > self.buffer.len() {
            self.compact();
            return Ok(None);
        }
        let Some(payload) = self.buffer.get(start..end) else {
            return Ok(None);
        };
        self.consumed = end;
        Ok(Some(Frame {
            channel: header.channel,
            payload,
        }))
    }

    /// Frees what has been consumed, called only when what remains is at most
    /// one partial frame: everything, at no cost, when nothing remains;
    /// otherwise the consumed prefix, once it outweighs both the threshold
    /// and the remainder, by moving that remainder to the front — after which
    /// `consumed` is zero until the frame completes, so it is moved once.
    fn compact(&mut self) {
        if self.consumed == 0 {
            return;
        }
        if self.consumed == self.buffer.len() {
            self.buffer.clear();
            self.consumed = 0;
            return;
        }
        let remaining = self.buffer.len().saturating_sub(self.consumed);
        if self.consumed >= COMPACTION_THRESHOLD && self.consumed >= remaining {
            self.buffer.drain(..self.consumed);
            self.consumed = 0;
        }
    }
}
