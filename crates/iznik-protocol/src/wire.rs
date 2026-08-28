//! The little-endian, length-prefixed primitives every codec in this crate
//! reads and writes with: a sink that is measured before it is filled, so an
//! oversize encoding is refused before anything is allocated, and a reader
//! that names what it could not read. The control messages use them now;
//! the session-model payloads of plan 0003 will. The refusals are
//! [`MessageError`]'s, the crate's one vocabulary for what a decoder found,
//! so this module and `message` lean on each other by design.

use crate::frame::MAXIMUM_PAYLOAD_LENGTH;
use crate::identity::PaneId;
use crate::message::{CHANNEL_CONTROL, MessageError, NO_DISCRIMINANT};

/// The byte that says an optional value is absent, and the boolean `false`.
pub(crate) const ABSENT: u8 = 0;

/// The byte that says an optional value follows, and the boolean `true`.
pub(crate) const PRESENT: u8 = 1;

/// Where an encoding goes: a byte count first, so an oversize message is
/// refused before anything is allocated, and then a buffer.
pub(crate) trait Sink {
    /// Appends bytes.
    fn put(&mut self, bytes: &[u8]);
}

impl Sink for usize {
    fn put(&mut self, bytes: &[u8]) {
        *self = self.saturating_add(bytes.len());
    }
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// Appends a length-delimited string or payload.
pub(crate) fn put_bytes(sink: &mut dyn Sink, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    sink.put(&length.to_le_bytes());
    sink.put(bytes);
}

/// Appends the count of the elements that follow. A count that does not fit
/// four bytes is written saturated, which [`encode`] then refuses as
/// oversize: no encoding this crate hands out carries a truncated count.
pub(crate) fn put_count(sink: &mut dyn Sink, count: usize) {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    sink.put(&count.to_le_bytes());
}

/// Appends a message tag and the pane id that follows it.
pub(crate) fn put_pane(sink: &mut dyn Sink, tag: u8, pane: PaneId) {
    sink.put(&[tag]);
    sink.put(&pane.0.to_le_bytes());
}

/// Measures an encoding, refuses it if it would not fit a frame, and only
/// then allocates and writes it.
///
/// # Errors
///
/// [`MessageError::Oversize`] when the encoding would exceed
/// [`MAXIMUM_PAYLOAD_LENGTH`].
pub(crate) fn encode(write: impl Fn(&mut dyn Sink)) -> Result<Vec<u8>, MessageError> {
    let mut length: usize = 0;
    write(&mut length);
    let maximum = usize::try_from(MAXIMUM_PAYLOAD_LENGTH).unwrap_or(usize::MAX);
    if length > maximum {
        return Err(MessageError::Oversize { length });
    }
    let mut out = Vec::with_capacity(length);
    write(&mut out);
    Ok(out)
}

/// A cursor over a message's bytes that remembers the discriminant every
/// refusal names.
pub(crate) struct Reader<'bytes> {
    /// The whole message.
    bytes: &'bytes [u8],
    /// How many bytes have been read.
    position: usize,
    /// The message's discriminant, read first.
    pub(crate) discriminant: u8,
}

impl<'bytes> Reader<'bytes> {
    /// A reader over a payload that carries no discriminant: the session-model
    /// encodings, whose first field is a value rather than a tag. Their
    /// refusals name [`NO_DISCRIMINANT`], which no message claims, so a
    /// refusal inside a model is never read as a refusal of a message.
    pub(crate) fn payload(bytes: &'bytes [u8]) -> Reader<'bytes> {
        Reader {
            bytes,
            position: 0,
            discriminant: NO_DISCRIMINANT,
        }
    }

    /// A reader positioned after the discriminant.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] for an empty payload.
    pub(crate) fn new(bytes: &'bytes [u8]) -> Result<Reader<'bytes>, MessageError> {
        let Some(discriminant) = bytes.first() else {
            return Err(MessageError::Truncated {
                discriminant: 0,
                needed: size_of::<u8>(),
                available: 0,
            });
        };
        Ok(Reader {
            bytes,
            position: size_of::<u8>(),
            discriminant: *discriminant,
        })
    }

    /// The next `needed` bytes.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when fewer are left.
    fn take(&mut self, needed: usize) -> Result<&'bytes [u8], MessageError> {
        let available = self.bytes.len().saturating_sub(self.position);
        let end = self.position.saturating_add(needed);
        let Some(taken) = self.bytes.get(self.position..end) else {
            return Err(MessageError::Truncated {
                discriminant: self.discriminant,
                needed,
                available,
            });
        };
        self.position = end;
        Ok(taken)
    }

    /// The next byte.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when none is left.
    pub(crate) fn byte(&mut self) -> Result<u8, MessageError> {
        let [byte] = self.array::<1>()?;
        Ok(byte)
    }

    /// The next `WIDTH` bytes as an array: the little-endian bytes of an
    /// integer, which the caller converts.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when fewer bytes are left.
    pub(crate) fn array<const WIDTH: usize>(&mut self) -> Result<[u8; WIDTH], MessageError> {
        let rest = self.bytes.get(self.position..).unwrap_or_default();
        let Some(chunk) = rest.first_chunk::<WIDTH>() else {
            return Err(MessageError::Truncated {
                discriminant: self.discriminant,
                needed: WIDTH,
                available: rest.len(),
            });
        };
        self.position = self.position.saturating_add(WIDTH);
        Ok(*chunk)
    }

    /// The count of the elements that follow. It is not trusted: a decoder
    /// reads that many elements and is stopped by the bytes running out, so a
    /// count nothing backs costs one refusal rather than an allocation.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when four bytes are not left.
    pub(crate) fn count(&mut self) -> Result<usize, MessageError> {
        let count = u32::from_le_bytes(self.array()?);
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    /// The next length-delimited bytes, owned.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when the length or the bytes are cut short.
    pub(crate) fn bytes(&mut self) -> Result<Vec<u8>, MessageError> {
        let length = u32::from_le_bytes(self.array()?);
        let taken = self.take(usize::try_from(length).unwrap_or(usize::MAX))?;
        Ok(taken.to_vec())
    }

    /// The next length-delimited string.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when it is cut short, [`MessageError::Utf8`]
    /// when it is not UTF-8.
    pub(crate) fn string(&mut self) -> Result<String, MessageError> {
        let discriminant = self.discriminant;
        String::from_utf8(self.bytes()?).map_err(|_error| MessageError::Utf8 { discriminant })
    }

    /// The next byte as a presence or boolean flag.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when none is left,
    /// [`MessageError::UnknownDiscriminant`] when it is neither 0 nor 1.
    pub(crate) fn flag(&mut self) -> Result<bool, MessageError> {
        match self.byte()? {
            ABSENT => Ok(false),
            PRESENT => Ok(true),
            other => Err(unknown(other)),
        }
    }

    /// Confirms nothing is left.
    ///
    /// # Errors
    ///
    /// [`MessageError::TrailingBytes`] when bytes follow the last field.
    pub(crate) fn finish(self) -> Result<(), MessageError> {
        let count = self.bytes.len().saturating_sub(self.position);
        if count == 0 {
            Ok(())
        } else {
            Err(MessageError::TrailingBytes {
                discriminant: self.discriminant,
                count,
            })
        }
    }
}

/// The refusal of a byte no variant claims.
pub(crate) fn unknown(discriminant: u8) -> MessageError {
    MessageError::UnknownDiscriminant {
        channel: CHANNEL_CONTROL,
        discriminant,
    }
}
