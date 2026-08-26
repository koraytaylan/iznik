//! The framed link over `tokio::io::duplex`: every frame arrives as sent on
//! every channel, the decoder resumes across every split the stream can
//! make, a frame is one vectored write of the caller's own bytes, the end of
//! the stream is `None` between frames and `Closed` inside one, an oversize
//! header is refused before its payload, the halves carry a thousand frames,
//! and the parts hand back exactly the half frame a read captured.

use std::io::IoSlice;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use iznik_link::framed::{FramedLink, LinkError};
use iznik_protocol::frame::{FrameError, FrameHeader, HEADER_LENGTH, MAXIMUM_PAYLOAD_LENGTH};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf, duplex};
use tokio::time::timeout;

/// The most a test waits for the peer.
const DEADLINE: Duration = Duration::from_secs(5);

/// A duplex buffer that never blocks a test's writes.
const ROOMY: usize = 4 * 1024 * 1024;

/// A frame, owned.
type Owned = (u8, Vec<u8>);

/// Every frame the link yields until the stream ends, owned.
///
/// # Errors
///
/// The link's error, or the deadline.
async fn receive_all(link: &mut FramedLink<DuplexStream>) -> Result<Vec<Owned>, String> {
    let mut frames = Vec::new();
    loop {
        let next = timeout(DEADLINE, link.next_frame())
            .await
            .map_err(|_elapsed| "the deadline passed waiting for a frame".to_owned())?
            .map_err(|error| error.to_string())?;
        match next {
            Some(frame) => frames.push((frame.channel, frame.payload.to_vec())),
            None => return Ok(frames),
        }
    }
}

/// Sends every frame, then drops the link so the peer sees the end.
///
/// # Errors
///
/// The link's error.
async fn send_all(mut link: FramedLink<DuplexStream>, frames: &[Owned]) -> Result<(), String> {
    for (channel, payload) in frames {
        link.send(*channel, payload)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Frames on every channel, including an empty payload and one of the
/// maximum length, arrive with the same channel and payload.
///
/// # Panics
///
/// When any frame differs, is missing, or the deadline passes.
#[tokio::test]
async fn framed_round_trips_every_channel_and_the_maximum_payload() {
    let (ours, theirs) = duplex(ROOMY);
    let mut expected: Vec<Owned> = (0..=u8::MAX)
        .map(|channel| (channel, vec![channel; usize::from(channel)]))
        .collect();
    expected.push((7, Vec::new()));
    let maximum = usize::try_from(MAXIMUM_PAYLOAD_LENGTH).expect("the maximum fits");
    expected.push((9, vec![0xa5; maximum]));
    let to_send = expected.clone();
    let sender = tokio::spawn(async move { send_all(FramedLink::new(theirs), &to_send).await });
    let mut link = FramedLink::new(ours);
    let received = receive_all(&mut link).await.expect("every frame arrives");
    sender
        .await
        .expect("the sender task ends")
        .expect("every frame is sent");
    assert_eq!(received.len(), expected.len());
    assert!(received == expected, "a frame differs");
}

/// With the duplex buffer sized so reads split at every boundary of a
/// multi-frame stream, the link yields the identical frames.
///
/// # Panics
///
/// When any buffer size yields other frames, or the deadline passes.
#[tokio::test]
async fn framed_resumes_across_every_split_of_the_stream() {
    let expected: Vec<Owned> = vec![
        (1, b"first".to_vec()),
        (2, Vec::new()),
        (3, b"a third frame with a longer payload".to_vec()),
        (255, vec![0; 64]),
    ];
    let stream_length = expected
        .iter()
        .fold(0_usize, |length, (_channel, payload)| {
            length
                .saturating_add(HEADER_LENGTH)
                .saturating_add(payload.len())
        });
    for size in 1..=stream_length {
        let (ours, theirs) = duplex(size);
        let to_send = expected.clone();
        let sender = tokio::spawn(async move { send_all(FramedLink::new(theirs), &to_send).await });
        let mut link = FramedLink::new(ours);
        let received = receive_all(&mut link).await.expect("every frame arrives");
        sender
            .await
            .expect("the sender task ends")
            .expect("every frame is sent");
        assert!(
            received == expected,
            "reads of at most {size} bytes yielded other frames"
        );
    }
}

/// A stream that counts vectored writes and remembers where the payload
/// slice of the last one pointed.
struct Counting {
    /// The stream written through.
    inner: DuplexStream,
    /// How many vectored writes were made.
    writes: Arc<AtomicUsize>,
    /// The address of the second slice of the last vectored write.
    payload_address: Arc<AtomicUsize>,
    /// How many slices the last vectored write carried.
    slice_count: Arc<AtomicUsize>,
    /// The length of the second slice of the last vectored write.
    payload_length: Arc<AtomicUsize>,
    /// The address the last read was given to fill.
    read_address: Arc<AtomicUsize>,
}

impl AsyncRead for Counting {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.read_address.store(
            buffer.initialize_unfilled().as_ptr().addr(),
            Ordering::SeqCst,
        );
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for Counting {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, bytes)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        slices: &[IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.slice_count.store(slices.len(), Ordering::SeqCst);
        if let Some(payload) = slices.get(1) {
            self.payload_address
                .store(payload.as_ptr().addr(), Ordering::SeqCst);
            self.payload_length.store(payload.len(), Ordering::SeqCst);
        }
        Pin::new(&mut self.inner).poll_write_vectored(context, slices)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

/// `send` issues exactly one vectored write, and the payload slice it hands
/// the stream is the caller's own buffer.
///
/// # Panics
///
/// When the write count is not one, or the payload was copied first.
#[tokio::test]
async fn framed_send_is_one_vectored_write_of_the_callers_bytes() {
    let (ours, _theirs) = duplex(ROOMY);
    let writes = Arc::new(AtomicUsize::new(0));
    let payload_address = Arc::new(AtomicUsize::new(0));
    let slice_count = Arc::new(AtomicUsize::new(0));
    let payload_length = Arc::new(AtomicUsize::new(0));
    let counting = Counting {
        inner: ours,
        writes: Arc::clone(&writes),
        payload_address: Arc::clone(&payload_address),
        slice_count: Arc::clone(&slice_count),
        payload_length: Arc::clone(&payload_length),
        read_address: Arc::new(AtomicUsize::new(0)),
    };
    let mut link = FramedLink::new(counting);
    let payload = vec![0x5a; 4096];
    link.send(3, &payload).await.expect("the frame is sent");
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "one vectored write per frame"
    );
    assert_eq!(
        payload_address.load(Ordering::SeqCst),
        payload.as_ptr().addr(),
        "the payload slice handed to the stream is the caller's buffer"
    );
    assert_eq!(
        slice_count.load(Ordering::SeqCst),
        2,
        "the header and the whole payload"
    );
    assert_eq!(payload_length.load(Ordering::SeqCst), payload.len());
}

/// A received frame is yielded from where the read landed: the payload's
/// address is the address the stream was given to fill, plus the header.
///
/// # Panics
///
/// When the payload was copied after the read, or the frame does not arrive.
#[tokio::test]
async fn framed_a_frame_is_yielded_where_the_read_landed() {
    let (ours, mut theirs) = duplex(ROOMY);
    let read_address = Arc::new(AtomicUsize::new(0));
    let counting = Counting {
        inner: ours,
        writes: Arc::new(AtomicUsize::new(0)),
        payload_address: Arc::new(AtomicUsize::new(0)),
        slice_count: Arc::new(AtomicUsize::new(0)),
        payload_length: Arc::new(AtomicUsize::new(0)),
        read_address: Arc::clone(&read_address),
    };
    let mut frame = Vec::new();
    iznik_protocol::frame::encode(2, b"landed here", &mut frame).expect("encodes");
    theirs
        .write_all(&frame)
        .await
        .expect("the frame is written");
    let mut link = FramedLink::new(counting);
    let yielded = timeout(DEADLINE, link.next_frame())
        .await
        .expect("no deadline")
        .expect("the frame decodes")
        .expect("a frame");
    assert_eq!(yielded.payload, b"landed here");
    assert_eq!(
        yielded.payload.as_ptr().addr(),
        read_address
            .load(Ordering::SeqCst)
            .checked_add(HEADER_LENGTH)
            .expect("fits"),
        "the payload sits where the read put it"
    );
}

/// After the peer closes, `next_frame` yields `None` after the last complete
/// frame.
///
/// # Panics
///
/// When the frames differ or the clean end is not `None`, or the deadline
/// passes.
#[tokio::test]
async fn framed_end_of_stream_between_frames_is_none() {
    let (ours, theirs) = duplex(ROOMY);
    let frames = vec![(1, b"one".to_vec()), (2, b"two".to_vec())];
    send_all(FramedLink::new(theirs), &frames)
        .await
        .expect("sent");
    let mut link = FramedLink::new(ours);
    let received = receive_all(&mut link)
        .await
        .expect("the stream ends cleanly");
    assert_eq!(received, frames);
    let again = timeout(DEADLINE, link.next_frame())
        .await
        .expect("no deadline")
        .expect("the end is still clean");
    assert!(again.is_none(), "the end of the stream is `None` again");
}

/// A stream closed mid-frame yields `Closed`.
///
/// # Panics
///
/// When the cut end is not `Closed`, or the deadline passes.
#[tokio::test]
async fn framed_end_of_stream_inside_a_frame_is_closed() {
    let (ours, mut theirs) = duplex(ROOMY);
    let mut header = Vec::new();
    FrameHeader {
        length: 10,
        channel: 1,
    }
    .write(&mut header);
    theirs
        .write_all(&header)
        .await
        .expect("the header is written");
    theirs
        .write_all(b"half")
        .await
        .expect("half a payload is written");
    drop(theirs);
    let mut link = FramedLink::new(ours);
    let outcome = timeout(DEADLINE, link.next_frame())
        .await
        .expect("no deadline");
    assert!(matches!(outcome, Err(LinkError::Closed)), "{outcome:?}");
}

/// A header over the maximum yields `Frame(Oversize)` before any payload is
/// read: the stream stays open and carries nothing after the header.
///
/// # Panics
///
/// When the refusal does not come, or waits for a payload.
#[tokio::test]
async fn framed_oversize_header_is_refused_before_any_payload() {
    let (ours, mut theirs) = duplex(ROOMY);
    let length = MAXIMUM_PAYLOAD_LENGTH
        .checked_add(1)
        .expect("one more fits a u32");
    let mut header = Vec::new();
    FrameHeader { length, channel: 1 }.write(&mut header);
    theirs
        .write_all(&header)
        .await
        .expect("the header is written");
    let mut link = FramedLink::new(ours);
    let outcome = timeout(DEADLINE, link.next_frame())
        .await
        .expect("the refusal does not wait for a payload");
    assert!(
        matches!(outcome, Err(LinkError::Frame(FrameError::Oversize { length: named })) if named == length),
        "{outcome:?}"
    );
}

/// After `split`, one task sending a thousand frames while another receives
/// them yields every frame, and dropping the writer ends the reader with
/// `None`. The duplex buffer is smaller than the thousand frames, so the
/// sender blocks on the receiver and the halves run concurrently; the
/// sending link's unused reading half is dropped first, because the stream
/// ends for the peer only when both halves are gone.
///
/// # Panics
///
/// When a frame is missing or differs, the end is not `None`, or the
/// deadline passes.
#[tokio::test]
async fn framed_split_halves_carry_a_thousand_frames_and_end_with_none() {
    let (ours, theirs) = duplex(64 * 1024);
    let frames: Vec<Owned> = (0..1000_u32)
        .map(|index| {
            let channel = u8::try_from(index % 255)
                .expect("fits")
                .checked_add(1)
                .expect("fits");
            (
                channel,
                index
                    .to_le_bytes()
                    .repeat(usize::try_from(index % 40).expect("fits")),
            )
        })
        .collect();
    let (unused_reader, mut writer) = FramedLink::new(theirs).split();
    drop(unused_reader);
    let to_send = frames.clone();
    let sender = tokio::spawn(async move {
        for (channel, payload) in &to_send {
            writer
                .send(*channel, payload)
                .await
                .map_err(|error| error.to_string())?;
        }
        drop(writer);
        Ok::<(), String>(())
    });
    let (mut reader, _writer) = FramedLink::new(ours).split();
    let mut received = Vec::new();
    loop {
        let next = timeout(DEADLINE, reader.next_frame())
            .await
            .expect("no deadline")
            .expect("the reader does not fail");
        match next {
            Some(frame) => received.push((frame.channel, frame.payload.to_vec())),
            None => break,
        }
    }
    sender
        .await
        .expect("the sender task ends")
        .expect("every frame is sent");
    assert_eq!(received.len(), frames.len());
    assert!(received == frames, "a frame differs");
}

/// `into_parts` after a read that captured one and a half frames returns
/// the stream and exactly the half frame's bytes, and a link built from those
/// parts yields the second frame whole.
///
/// # Panics
///
/// When the parts are not exactly the half frame, or the rebuilt link does
/// not yield the second frame.
#[tokio::test]
async fn framed_into_parts_hands_back_exactly_the_half_frame() {
    let (ours, mut theirs) = duplex(ROOMY);
    let mut first = Vec::new();
    iznik_protocol::frame::encode(1, b"first", &mut first).expect("encodes");
    let mut second = Vec::new();
    iznik_protocol::frame::encode(2, b"the second frame", &mut second).expect("encodes");
    let (second_head, second_tail) = second.split_at(HEADER_LENGTH.checked_add(3).expect("fits"));
    theirs
        .write_all(&[first.as_slice(), second_head].concat())
        .await
        .expect("one and a half frames are written");
    let mut link = FramedLink::new(ours);
    let yielded = timeout(DEADLINE, link.next_frame())
        .await
        .expect("no deadline")
        .expect("the first frame decodes")
        .map(|frame| (frame.channel, frame.payload.to_vec()));
    assert_eq!(yielded, Some((1, b"first".to_vec())));
    let (stream, pending) = link.into_parts();
    assert_eq!(pending, second_head, "exactly the half frame's bytes");
    theirs
        .write_all(second_tail)
        .await
        .expect("the rest is written");
    let mut rebuilt = FramedLink::from_parts(stream, &pending);
    let second_yielded = timeout(DEADLINE, rebuilt.next_frame())
        .await
        .expect("no deadline")
        .expect("the second frame decodes")
        .map(|frame| (frame.channel, frame.payload.to_vec()));
    assert_eq!(second_yielded, Some((2, b"the second frame".to_vec())));
}
