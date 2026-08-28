//! Streaming zstd under a framed link, primed with the protocol's dictionary
//! and engaged from a plain link's parts once both `Hello`s agree.
//!
//! One compressor per connection, not one per frame. A compressor that starts
//! afresh on every frame learns nothing and inflates a keystroke — a one-byte
//! frame becomes a whole zstd frame header — while one that spans the
//! connection has the whole session as its window, and the dictionary as its
//! head start. The cost of spanning frames is that the encoder holds bytes
//! back until it has a block worth compressing, so this flushes it after every
//! write: nothing a client is waiting for ever waits in a buffer here.
//!
//! Two committed numbers decide whether the capability stays advertised at
//! all: [`MINIMUM_CORPUS_RATIO`] and [`SMALL_FRAME_LATENCY_CEILING`]. Trading
//! throughput for perceptible keystroke latency would be a bad bargain, and
//! the second number is what would catch it.

use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use iznik_protocol::dictionary::COMPRESSION_DICTIONARY;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use zstd::stream::raw::{Decoder, Encoder, Operation, OutBuffer};

use crate::framed::FramedLink;

/// The least the fidelity corpus must compress by for the capability to be
/// worth advertising. Terminal output is escape sequences and repeated words;
/// anything under threefold would not pay for the code.
pub const MINIMUM_CORPUS_RATIO: f64 = 3.0;

/// The most compression may add to a one-byte frame's round trip at the
/// ninety-ninth percentile. A keystroke echo is the one latency a person can
/// feel, and a millisecond is already generous beside a network.
pub const SMALL_FRAME_LATENCY_CEILING: Duration = Duration::from_millis(1);

/// The level the link compresses at. Three is zstd's default: on terminal
/// output it is within a few percent of the slowest levels, and its cost per
/// byte is what a keystroke pays.
const COMPRESSION_LEVEL: i32 = 3;

/// The scratch one encode or decode step writes into before its bytes are
/// appended to a buffer. Smaller than a frame only means more steps.
const WORK_LENGTH: usize = 16 * 1024;

/// How many compressed bytes are taken from the stream at a time.
const READ_LENGTH: usize = 16 * 1024;

/// A byte stream with zstd between it and its user: what is written is
/// compressed, what is read is decompressed, and both ends carry the state of
/// one connection.
///
/// A `Pending` write is retried with the bytes it was given, as
/// [`tokio::io::AsyncWriteExt`] does, so the compressor never sees the same
/// bytes twice.
pub struct ZstdStream<Stream> {
    /// The stream underneath, carrying compressed bytes.
    stream: Stream,
    /// The encoder, primed with the dictionary.
    encoder: Encoder<'static>,
    /// The decoder, primed with the dictionary.
    decoder: Decoder<'static>,
    /// Compressed bytes waiting to be written to the stream.
    outgoing: Vec<u8>,
    /// How much of `outgoing` has gone out.
    sent: usize,
    /// Compressed bytes read from the stream and not yet decoded.
    incoming: Vec<u8>,
    /// How much of `incoming` has been decoded.
    decoded: usize,
    /// Decompressed bytes waiting to be read.
    plain: Vec<u8>,
    /// How much of `plain` has been handed out.
    taken: usize,
    /// What a write took from its caller and has not yet reported, held
    /// across a `Pending` so a retry does not compress the same bytes twice.
    /// It carries the length it took, so that a retry of a *different* write —
    /// which means the first was abandoned, and `FramedLink::send` says what
    /// that costs — is compressed rather than silently answered with the
    /// abandoned write's count.
    accepted: Option<usize>,
    /// Whether the zstd frame has been finished; finishing twice is not
    /// something the encoder is asked to survive.
    finished: bool,
    /// Whether the frame being decoded has reached its end. A stream that
    /// stops in the middle of one is a truncation, not a clean close, and
    /// saying otherwise would hand a client half a screen as if it were all
    /// of it.
    ended: bool,
    /// The scratch one encode or decode step writes into, kept so that a
    /// keystroke does not allocate.
    work: Vec<u8>,
    /// Where compressed bytes land as they come off the stream.
    reading: Vec<u8>,
}

impl<Stream> core::fmt::Debug for ZstdStream<Stream> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // zstd's contexts are opaque, so what is printable is how much is
        // held at each stage — which is what a stuck link is diagnosed by.
        formatter
            .debug_struct("ZstdStream")
            .field("outgoing", &self.outgoing.len().saturating_sub(self.sent))
            .field(
                "incoming",
                &self.incoming.len().saturating_sub(self.decoded),
            )
            .field("plain", &self.plain.len().saturating_sub(self.taken))
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

/// The least a buffer must have given up before its remainder is moved down.
/// Below this the move costs more than the bytes it reclaims, and a large
/// frame would be moved once per step of the decoder.
const COMPACT_THRESHOLD: usize = 4 * 1024;

/// Drops what a buffer has already given up, once there is enough of it to be
/// worth the move.
fn compact(buffer: &mut Vec<u8>, consumed: &mut usize) {
    if *consumed == 0 {
        return;
    }
    if *consumed >= buffer.len() {
        buffer.clear();
    } else if *consumed < COMPACT_THRESHOLD {
        return;
    } else {
        let _removed = buffer.drain(..*consumed);
    }
    *consumed = 0;
}

impl<Stream> ZstdStream<Stream> {
    /// A compressed stream over a plain one, starting with `leftover` — the
    /// peer's first compressed bytes, read past its `Hello` by the plain link
    /// and handed on rather than lost.
    ///
    /// # Errors
    ///
    /// Whatever zstd says when it cannot build a context around the
    /// dictionary, which is a fault in this binary rather than in the link.
    pub fn new(stream: Stream, leftover: Vec<u8>) -> io::Result<ZstdStream<Stream>> {
        Ok(ZstdStream {
            stream,
            encoder: Encoder::with_dictionary(COMPRESSION_LEVEL, COMPRESSION_DICTIONARY)?,
            decoder: Decoder::with_dictionary(COMPRESSION_DICTIONARY)?,
            outgoing: Vec::new(),
            sent: 0,
            incoming: leftover,
            decoded: 0,
            plain: Vec::new(),
            taken: 0,
            accepted: None,
            finished: false,
            ended: true,
            work: vec![0; WORK_LENGTH],
            reading: vec![0; READ_LENGTH],
        })
    }

    /// Compresses `bytes` into `outgoing`, holding back whatever the encoder
    /// is not ready to emit.
    ///
    /// # Errors
    ///
    /// Whatever the encoder says.
    fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        let ZstdStream {
            encoder,
            outgoing,
            work,
            ..
        } = self;
        let mut offset = 0;
        while offset < bytes.len() {
            let input = bytes.get(offset..).unwrap_or_default();
            let status = encoder.run_on_buffers(input, work)?;
            offset = offset.saturating_add(status.bytes_read);
            outgoing.extend_from_slice(work.get(..status.bytes_written).unwrap_or_default());
            if status.bytes_read == 0 && status.bytes_written == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Pushes everything the encoder is holding into `outgoing`, so nothing a
    /// caller has written waits inside zstd for a block to fill.
    ///
    /// # Errors
    ///
    /// Whatever the encoder says.
    fn flush_encoder(&mut self) -> io::Result<()> {
        let ZstdStream {
            encoder,
            outgoing,
            work,
            ..
        } = self;
        loop {
            let mut out = OutBuffer::around(work);
            let remaining = encoder.flush(&mut out)?;
            let written = out.pos();
            outgoing.extend_from_slice(work.get(..written).unwrap_or_default());
            if remaining == 0 || written == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Ends the zstd frame, once.
    ///
    /// # Errors
    ///
    /// Whatever the encoder says.
    fn finish_encoder(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        let ZstdStream {
            encoder,
            outgoing,
            work,
            ..
        } = self;
        loop {
            let mut out = OutBuffer::around(work);
            let remaining = encoder.finish(&mut out, true)?;
            let written = out.pos();
            outgoing.extend_from_slice(work.get(..written).unwrap_or_default());
            if remaining == 0 || written == 0 {
                break;
            }
        }
        self.finished = true;
        Ok(())
    }

    /// Decompresses what has arrived, and says whether it produced anything.
    ///
    /// # Errors
    ///
    /// Whatever the decoder says, which for a corrupt stream is what makes a
    /// wrong byte a failure rather than a wrong terminal.
    fn decode(&mut self) -> io::Result<bool> {
        let ZstdStream {
            decoder,
            incoming,
            decoded,
            plain,
            work,
            ended,
            ..
        } = self;
        let mut produced = false;
        loop {
            let input = incoming.get(*decoded..).unwrap_or_default();
            let status = decoder.run_on_buffers(input, work)?;
            *decoded = decoded.saturating_add(status.bytes_read);
            plain.extend_from_slice(work.get(..status.bytes_written).unwrap_or_default());
            *ended = status.remaining == 0;
            if status.bytes_written > 0 {
                produced = true;
                break;
            }
            // Nothing read and nothing written is a decoder that has given up
            // everything it holds and wants bytes it does not have.
            if status.bytes_read == 0 {
                break;
            }
        }
        compact(incoming, decoded);
        Ok(produced)
    }
}

impl<Stream: AsyncWrite + Unpin> ZstdStream<Stream> {
    /// Pushes what is compressed out to the stream.
    fn poll_drain(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let ZstdStream {
            stream,
            outgoing,
            sent,
            ..
        } = self;
        while *sent < outgoing.len() {
            let waiting = outgoing.get(*sent..).unwrap_or_default();
            let count = ready!(Pin::new(&mut *stream).poll_write(context, waiting))?;
            if count == 0 {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "the stream accepted no compressed bytes",
                )));
            }
            *sent = sent.saturating_add(count);
        }
        compact(outgoing, sent);
        Poll::Ready(Ok(()))
    }

    /// Compresses what a write was given, flushes the encoder after it, and
    /// reports what it took once the bytes are on the stream.
    fn poll_send(
        &mut self,
        context: &mut Context<'_>,
        slices: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let offered = slices
            .iter()
            .fold(0_usize, |total, slice| total.saturating_add(slice.len()));
        if self.accepted != Some(offered) {
            // Either nothing is pending, or what is pending was a different
            // write and so was abandoned — which `FramedLink::send` documents
            // as leaving a torn frame. Better a torn frame than this one
            // silently answered with the abandoned one's count.
            for slice in slices {
                self.feed(slice)?;
            }
            self.flush_encoder()?;
            self.accepted = Some(offered);
        }
        match self.poll_drain(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => {
                self.accepted = None;
                return Poll::Ready(Err(error));
            }
            Poll::Ready(Ok(())) => {}
        }
        self.accepted = None;
        Poll::Ready(Ok(offered))
    }
}

impl<Stream: AsyncWrite + Unpin> AsyncWrite for ZstdStream<Stream> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.get_mut().poll_send(context, &[IoSlice::new(buffer)])
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        // The slices are used where they are, so a keystroke does not allocate
        // a list of them on its way to the encoder.
        self.get_mut().poll_send(context, buffers)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.accepted = None;
        this.flush_encoder()?;
        ready!(this.poll_drain(context))?;
        Pin::new(&mut this.stream).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.accepted = None;
        this.finish_encoder()?;
        ready!(this.poll_drain(context))?;
        Pin::new(&mut this.stream).poll_shutdown(context)
    }
}

impl<Stream: AsyncRead + Unpin> AsyncRead for ZstdStream<Stream> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let ready_bytes = this.plain.get(this.taken..).unwrap_or_default();
            if !ready_bytes.is_empty() {
                let count = ready_bytes.len().min(buffer.remaining());
                buffer.put_slice(ready_bytes.get(..count).unwrap_or_default());
                this.taken = this.taken.saturating_add(count);
                compact(&mut this.plain, &mut this.taken);
                return Poll::Ready(Ok(()));
            }
            compact(&mut this.plain, &mut this.taken);
            if this.decode()? {
                continue;
            }
            let ZstdStream {
                stream,
                incoming,
                reading,
                ended,
                ..
            } = this;
            let mut arrived = ReadBuf::new(reading);
            ready!(Pin::new(&mut *stream).poll_read(context, &mut arrived))?;
            let taken = arrived.filled();
            if taken.is_empty() {
                if *ended {
                    return Poll::Ready(Ok(()));
                }
                // A stream that stops inside a zstd frame has lost bytes. The
                // link above only sees a torn *plain* frame, so a truncation
                // landing on a plaintext boundary would read as an orderly
                // close and hand a client half a screen as if it were all.
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the stream ended inside a compressed frame",
                )));
            }
            *ended = false;
            incoming.extend_from_slice(taken);
        }
    }
}

/// The compressed link both ends build from the parts of their plain one,
/// once both `Hello`s carried `Capabilities::ZSTD`.
///
/// `leftover` is the peer's first compressed bytes, which the plain link read
/// past its `Hello` and hands on rather than lose.
///
/// # Errors
///
/// Whatever zstd says when it cannot build a context around the dictionary,
/// which is a fault in this binary rather than in the link.
pub fn compressed<Stream: AsyncRead + AsyncWrite + Unpin>(
    stream: Stream,
    leftover: Vec<u8>,
) -> io::Result<FramedLink<ZstdStream<Stream>>> {
    Ok(FramedLink::new(ZstdStream::new(stream, leftover)?))
}
