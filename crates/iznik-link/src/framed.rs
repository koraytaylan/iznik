//! Frames over any duplex byte stream — a unix socket, an SSH child's
//! standard streams, an in-memory pipe: one vectored write per frame out, so
//! a pane byte is copied from the caller's buffer to the socket and nowhere
//! else; a resuming decoder in, fed once from the read buffer and lending
//! each frame out of its own — the one copy a decoder that lent its spare
//! capacity to the read would remove, a frame-codec follow-up; split halves,
//! because every real user reads on one task and writes on another; and the
//! parts a compression layer is built from once both `Hello`s agree.

use core::fmt::{self, Display, Formatter};
use std::io::{self, IoSlice};

use iznik_protocol::frame::{Frame, FrameDecoder, FrameError, FrameHeader, MAXIMUM_PAYLOAD_LENGTH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

/// How many bytes one read asks the stream for: a socket's send buffer is
/// of this order, so a full one drains in one read, and the read buffer is
/// sixty-four kibibytes.
const READ_LENGTH: usize = 64 * 1024;

/// Why a link could not send or receive.
#[derive(Debug)]
pub enum LinkError {
    /// The stream failed.
    Io {
        /// What the operating system said.
        source: io::Error,
    },
    /// The bytes do not frame.
    Frame(FrameError),
    /// The peer closed the stream in the middle of a frame.
    Closed,
}

impl Display for LinkError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Io { source } => write!(formatter, "the stream failed: {source}"),
            LinkError::Frame(source) => write!(formatter, "the bytes do not frame: {source}"),
            LinkError::Closed => write!(formatter, "the peer closed the stream inside a frame"),
        }
    }
}

impl std::error::Error for LinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LinkError::Io { source } => Some(source),
            LinkError::Frame(source) => Some(source),
            LinkError::Closed => None,
        }
    }
}

impl From<io::Error> for LinkError {
    fn from(source: io::Error) -> LinkError {
        LinkError::Io { source }
    }
}

impl From<FrameError> for LinkError {
    fn from(source: FrameError) -> LinkError {
        LinkError::Frame(source)
    }
}

/// The sending side's state: the one header buffer every frame reuses.
#[derive(Debug, Default)]
struct Sending {
    /// The five header bytes of the frame being sent.
    header: Vec<u8>,
}

impl Sending {
    /// Writes one frame in one vectored write — the header from this buffer,
    /// the payload from the caller's — retrying only what a short write left.
    /// Not cancel-safe: a `send` dropped after a short write leaves a torn
    /// frame, and the stream is corrupt from there.
    ///
    /// # Errors
    ///
    /// [`LinkError::Frame`] with [`FrameError::Oversize`] for a payload over
    /// [`MAXIMUM_PAYLOAD_LENGTH`], before anything is written;
    /// [`LinkError::Io`] when the stream fails or accepts nothing.
    async fn send<Writer: AsyncWrite + Unpin>(
        &mut self,
        stream: &mut Writer,
        channel: u8,
        payload: &[u8],
    ) -> Result<(), LinkError> {
        let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        if length > MAXIMUM_PAYLOAD_LENGTH {
            return Err(FrameError::Oversize { length }.into());
        }
        self.header.clear();
        FrameHeader { length, channel }.write(&mut self.header);
        let total = self.header.len().saturating_add(payload.len());
        let mut slices = [IoSlice::new(&self.header), IoSlice::new(payload)];
        let mut remaining: &mut [IoSlice<'_>] = &mut slices;
        let mut written: usize = 0;
        while written < total {
            let count = stream.write_vectored(remaining).await?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "the stream accepted no bytes of a frame",
                )
                .into());
            }
            // A stream that claims more than it was given must not drive
            // `advance_slices` past the end, where it would panic.
            let count = count.min(total.saturating_sub(written));
            written = written.saturating_add(count);
            IoSlice::advance_slices(&mut remaining, count);
        }
        Ok(())
    }
}

/// The receiving side's state: a read buffer and the decoder it feeds.
#[derive(Debug)]
struct Receiving {
    /// Where one read lands before its bytes are pushed to the decoder.
    buffer: Vec<u8>,
    /// The bytes past the last yielded frame, and the frames they hold.
    decoder: FrameDecoder,
}

impl Receiving {
    /// A receiver holding `pending` bytes already read past the last frame.
    fn new(pending: &[u8]) -> Receiving {
        let mut decoder = FrameDecoder::new();
        decoder.push(pending);
        Receiving {
            buffer: vec![0; READ_LENGTH],
            decoder,
        }
    }

    /// Reads until a frame is whole, then yields it borrowed from the decoder;
    /// `None` when the stream ends between frames. Cancel-safe: a read that
    /// is dropped has taken no bytes, and everything after a read is
    /// synchronous.
    ///
    /// # Errors
    ///
    /// [`LinkError::Frame`] with [`FrameError::Oversize`] as soon as a header
    /// names a length over the maximum, before its payload is read;
    /// [`LinkError::Closed`] when the stream ends inside a frame;
    /// [`LinkError::Io`] when the stream fails.
    async fn next<Reader: AsyncRead + Unpin>(
        &mut self,
        stream: &mut Reader,
    ) -> Result<Option<Frame<'_>>, LinkError> {
        while !self.decoder.ready()? {
            let count = stream.read(&mut self.buffer).await?;
            if count == 0 {
                return if self.decoder.pending().is_empty() {
                    Ok(None)
                } else {
                    Err(LinkError::Closed)
                };
            }
            self.decoder
                .push(self.buffer.get(..count).unwrap_or_default());
        }
        // `ready` said a frame waits and nothing was pushed since, so this
        // is `Some`; were it not, it would read as a clean end of stream.
        Ok(self.decoder.next_frame()?)
    }
}

/// Frames over a duplex byte stream, read and written from one place.
#[derive(Debug)]
pub struct FramedLink<Stream: AsyncRead + AsyncWrite + Unpin> {
    /// The stream.
    stream: Stream,
    /// The sending side.
    sending: Sending,
    /// The receiving side.
    receiving: Receiving,
}

impl<Stream: AsyncRead + AsyncWrite + Unpin> FramedLink<Stream> {
    /// A link over a fresh stream.
    pub fn new(stream: Stream) -> FramedLink<Stream> {
        FramedLink::from_parts(stream, &[])
    }

    /// The inverse of [`FramedLink::into_parts`] over the same byte stream:
    /// a link that begins with bytes already read from it. A layer that
    /// transforms the stream takes the leftover itself instead, because those
    /// bytes are its to decode.
    pub fn from_parts(stream: Stream, pending: &[u8]) -> FramedLink<Stream> {
        FramedLink {
            stream,
            sending: Sending::default(),
            receiving: Receiving::new(pending),
        }
    }

    /// Sends one frame: header and payload in one vectored write. Not
    /// cancel-safe: dropped after a short write, it leaves a torn frame and
    /// the stream is corrupt from there.
    ///
    /// # Errors
    ///
    /// [`LinkError::Frame`] for an oversize payload, before anything is
    /// written; [`LinkError::Io`] when the stream fails.
    pub async fn send(&mut self, channel: u8, payload: &[u8]) -> Result<(), LinkError> {
        self.sending.send(&mut self.stream, channel, payload).await
    }

    /// The next frame, borrowed from the link; `None` at a clean end of
    /// stream. Cancel-safe: a dropped call has taken no bytes.
    ///
    /// # Errors
    ///
    /// [`LinkError::Frame`] for an oversize header, before its payload is
    /// read; [`LinkError::Closed`] when the stream ends inside a frame;
    /// [`LinkError::Io`] when the stream fails.
    pub async fn next_frame(&mut self) -> Result<Option<Frame<'_>>, LinkError> {
        self.receiving.next(&mut self.stream).await
    }

    /// The two halves, so one task reads while another writes.
    pub fn split(self) -> (FrameReader<Stream>, FrameWriter<Stream>) {
        let (read, write) = tokio::io::split(self.stream);
        (
            FrameReader {
                half: read,
                receiving: self.receiving,
            },
            FrameWriter {
                half: write,
                sending: self.sending,
            },
        )
    }

    /// The stream and the bytes already read past the last yielded frame,
    /// which a layer put under a new link must be given first.
    pub fn into_parts(self) -> (Stream, Vec<u8>) {
        let pending = self.receiving.decoder.pending().to_vec();
        (self.stream, pending)
    }
}

/// The reading half of a split link. Dropping a half releases its share of
/// the stream; the stream is dropped, and the peer sees the end, when both
/// halves are gone — there is no half-close.
#[derive(Debug)]
pub struct FrameReader<Stream> {
    /// The stream's read half.
    half: ReadHalf<Stream>,
    /// The receiving side.
    receiving: Receiving,
}

impl<Stream: AsyncRead + Unpin> FrameReader<Stream> {
    /// The next frame, borrowed from the reader; `None` at a clean end of
    /// stream.
    ///
    /// # Errors
    ///
    /// As [`FramedLink::next_frame`].
    pub async fn next_frame(&mut self) -> Result<Option<Frame<'_>>, LinkError> {
        self.receiving.next(&mut self.half).await
    }
}

/// The writing half of a split link. Dropping it does not end the stream
/// while the reading half lives; see [`FrameReader`].
#[derive(Debug)]
pub struct FrameWriter<Stream> {
    /// The stream's write half.
    half: WriteHalf<Stream>,
    /// The sending side.
    sending: Sending,
}

impl<Stream: AsyncWrite + Unpin> FrameWriter<Stream> {
    /// Sends one frame: header and payload in one vectored write.
    ///
    /// # Errors
    ///
    /// As [`FramedLink::send`].
    pub async fn send(&mut self, channel: u8, payload: &[u8]) -> Result<(), LinkError> {
        self.sending.send(&mut self.half, channel, payload).await
    }
}
