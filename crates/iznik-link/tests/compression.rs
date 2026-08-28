//! The compressed link held to the two numbers that decide whether it is worth
//! having, and to the behaviour those numbers assume: every corpus construct
//! comes back byte-identical, the peer's first compressed bytes are not lost
//! when the link is rebuilt around them, one compressor spans the connection
//! rather than starting afresh per frame, and nothing a caller has written
//! waits inside zstd for a block to fill.

use std::error::Error;
use std::time::{Duration, Instant};

use iznik_link::compression::{
    MINIMUM_CORPUS_RATIO, SMALL_FRAME_LATENCY_CEILING, ZstdStream, compressed,
};
use iznik_link::framed::FramedLink;
use iznik_testkit::corpus;
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

/// How much a duplex pipe holds; more than any case here sends at once, so a
/// test never deadlocks on a full pipe instead of failing on its assertion.
const PIPE: usize = 1 << 20;

/// The channel every case sends on; any but the control channel would do.
const CHANNEL: u8 = 1;

/// How many samples the latency figure is taken over.
const SAMPLES: usize = 10_000;

/// How many one-byte frames the streaming case sends.
const KEYSTROKES: usize = 1_000;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(20);

/// Why a measurement could not be taken.
type Failure = Box<dyn Error>;

/// A byte count as a number a ratio can be taken with.
fn as_number(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The ninety-ninth percentile of a sorted list of measurements, or nothing
/// at all when there were none.
fn percentile(sorted: &[Duration]) -> Duration {
    let place = sorted.len().saturating_mul(99) / 100;
    sorted
        .get(place.min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}

/// The middle of a sorted list of measurements, or nothing at all when there
/// were none.
fn median(sorted: &[Duration]) -> Duration {
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}

/// Every chunk of every corpus construct, as the frames a pane's output would
/// arrive in.
fn corpus_frames() -> Vec<Vec<u8>> {
    corpus::constructs()
        .into_iter()
        .flat_map(|construct| construct.chunks)
        .collect()
}

/// Every corpus construct comes back byte-identical through a compressed pair.
///
/// # Panics
///
/// When a frame does not survive, or the case does not finish in time.
#[tokio::test]
async fn compression_every_corpus_frame_survives_the_round_trip() {
    let frames = corpus_frames();
    let sent = frames.clone();
    let case = async move {
        let (here, there) = duplex(PIPE);
        let mut sender = compressed(here, Vec::new()).expect("a compressed link");
        let mut receiver = compressed(there, Vec::new()).expect("a compressed link");
        for (index, payload) in sent.iter().enumerate() {
            sender.send(CHANNEL, payload).await.expect("a frame goes");
            let frame = receiver
                .next_frame()
                .await
                .expect("a frame arrives")
                .expect("the link is open");
            assert_eq!(frame.channel, CHANNEL, "frame {index}");
            assert_eq!(frame.payload, payload.as_slice(), "frame {index}");
        }
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the round trip finishes");
}

/// The peer's first compressed bytes, read past its `Hello` by the plain link,
/// are handed to the compressed one rather than lost.
///
/// # Panics
///
/// When the first compressed frame does not arrive whole.
#[tokio::test]
async fn compression_the_leftover_of_a_plain_link_is_not_lost() {
    let case = async {
        let (here, there) = duplex(PIPE);
        // The peer says hello in the clear and then switches, all before this
        // side reads anything: its first compressed bytes land in the plain
        // link's buffer behind the `Hello`.
        let mut peer = FramedLink::new(there);
        peer.send(CHANNEL, b"hello").await.expect("a plain frame");
        let (peer_stream, peer_leftover) = peer.into_parts();
        let mut talking = compressed(peer_stream, peer_leftover).expect("a compressed link");
        talking
            .send(CHANNEL, b"the first compressed frame")
            .await
            .expect("a compressed frame");
        talking
            .send(CHANNEL, b"and the second")
            .await
            .expect("a compressed frame");

        let mut plain = FramedLink::new(here);
        let greeting = plain
            .next_frame()
            .await
            .expect("a frame arrives")
            .expect("the link is open");
        assert_eq!(greeting.payload, b"hello", "the plain frame");
        let (stream, leftover) = plain.into_parts();
        assert!(
            !leftover.is_empty(),
            "the peer's compressed bytes were read past the hello"
        );

        let mut link = compressed(stream, leftover).expect("a compressed link");
        let first = link
            .next_frame()
            .await
            .expect("a frame arrives")
            .expect("the link is open");
        assert_eq!(first.payload, b"the first compressed frame", "the first");
        let second = link
            .next_frame()
            .await
            .expect("a frame arrives")
            .expect("the link is open");
        assert_eq!(second.payload, b"and the second", "the second");
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the leftover case finishes");
}

/// A single frame is readable on the other side without any further frame
/// being sent, so nothing a caller has written waits inside zstd.
///
/// # Panics
///
/// When the frame does not arrive on its own.
#[tokio::test]
async fn compression_a_frame_is_flushed_not_buffered() {
    let case = async {
        let (here, there) = duplex(PIPE);
        let mut sender = compressed(here, Vec::new()).expect("a compressed link");
        let mut receiver = compressed(there, Vec::new()).expect("a compressed link");
        sender.send(CHANNEL, b"one").await.expect("a frame goes");
        let frame = receiver
            .next_frame()
            .await
            .expect("a frame arrives")
            .expect("the link is open");
        assert_eq!(frame.payload, b"one", "the only frame sent");
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the flush case finishes");
}

/// One compressor spans the connection: a thousand one-byte frames cost far
/// less than a thousand compressions that each start afresh.
///
/// # Panics
///
/// When streaming is no better than compressing each frame on its own.
#[tokio::test]
async fn compression_streams_rather_than_starting_afresh_per_frame() {
    let case = async {
        let streamed = wire_bytes(&vec![vec![b'k']; KEYSTROKES])
            .await
            .expect("a measurement");
        let mut apart = 0_usize;
        for _index in 0..KEYSTROKES {
            apart = apart.saturating_add(wire_bytes(&[vec![b'k']]).await.expect("a measurement"));
        }
        assert!(
            streamed < apart,
            "streaming cost {streamed} bytes and starting afresh {apart}"
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the streaming case finishes");
}

/// How many bytes these frames put on the wire through a compressed link.
///
/// # Errors
///
/// When the link cannot be built, or the pipe fails.
async fn wire_bytes(frames: &[Vec<u8>]) -> Result<usize, Failure> {
    let (here, there) = duplex(PIPE);
    let mut link = compressed(here, Vec::new())?;
    for payload in frames {
        link.send(CHANNEL, payload).await?;
    }
    link.into_parts().0.shutdown().await?;
    let mut wire = Vec::new();
    let mut reader = there;
    let _read = reader.read_to_end(&mut wire).await?;
    Ok(wire.len())
}

/// How many bytes these frames put on the wire through a plain link.
///
/// # Errors
///
/// When the pipe fails.
async fn plain_wire_bytes(frames: &[Vec<u8>]) -> Result<usize, Failure> {
    let (here, there) = duplex(PIPE);
    let mut link = FramedLink::new(here);
    for payload in frames {
        link.send(CHANNEL, payload).await?;
    }
    link.into_parts().0.shutdown().await?;
    let mut wire = Vec::new();
    let mut reader = there;
    let _read = reader.read_to_end(&mut wire).await?;
    Ok(wire.len())
}

/// How many bytes these go on to the wire as, through the compressor with one
/// flush at the end: what the dictionary and the codec do to them, with no
/// framing and no per-frame flush in the way.
///
/// # Errors
///
/// When the compressor cannot be built, or the pipe fails.
async fn stream_bytes(bytes: &[u8]) -> Result<usize, Failure> {
    let (here, there) = duplex(PIPE);
    let mut stream = ZstdStream::new(here, Vec::new())?;
    stream.write_all(bytes).await?;
    stream.shutdown().await?;
    let mut wire = Vec::new();
    let mut reader = there;
    let _read = reader.read_to_end(&mut wire).await?;
    Ok(wire.len())
}

/// The corpus compresses by at least the ratio the capability is kept for, and
/// the message carries what it costs as the link actually frames it too —
/// which is the other half of the trade, and much the worse half on frames
/// this small.
///
/// # Panics
///
/// When it does not, with every measured number in the message.
#[tokio::test]
async fn compression_the_corpus_ratio_earns_the_capability() {
    let case = async {
        let frames = corpus_frames();
        let bytes: Vec<u8> = frames.iter().flatten().copied().collect();
        let raw = bytes.len();
        let bulk = stream_bytes(&bytes).await.expect("a measurement");
        let on_the_wire = wire_bytes(&frames).await.expect("a measurement");
        let plain = plain_wire_bytes(&frames).await.expect("a measurement");
        let ratio = as_number(raw) / as_number(bulk);
        let saved = as_number(plain) / as_number(on_the_wire);
        assert!(
            ratio >= MINIMUM_CORPUS_RATIO,
            "the corpus of {raw} bytes compresses to {bulk}, a ratio of {ratio:.3}, under the \
             {MINIMUM_CORPUS_RATIO} the capability is kept for"
        );
        assert!(
            on_the_wire < plain,
            "as the link frames it — {} frames, a flush after each — the corpus costs {on_the_wire} \
             bytes against {plain} uncompressed, so compression saves {saved:.3} times; the flush \
             a keystroke needs is what the rest of the {ratio:.3} goes on",
            frames.len()
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the ratio case finishes");
}

/// Compression adds no latency a person could feel to a one-byte frame.
///
/// # Panics
///
/// When it adds more than the ceiling, with the distribution in the message.
#[tokio::test]
async fn compression_small_frame_latency() {
    let case = async {
        let mut compressed_times = Vec::with_capacity(SAMPLES);
        let (here, there) = duplex(PIPE);
        let mut sender = compressed(here, Vec::new()).expect("a compressed link");
        let mut receiver = compressed(there, Vec::new()).expect("a compressed link");
        for _index in 0..SAMPLES {
            let at = Instant::now();
            sender.send(CHANNEL, b"k").await.expect("a keystroke goes");
            let frame = receiver
                .next_frame()
                .await
                .expect("it arrives")
                .expect("the link is open");
            assert_eq!(frame.payload, b"k", "a keystroke");
            compressed_times.push(at.elapsed());
        }

        let mut plain_times = Vec::with_capacity(SAMPLES);
        let (upstream, downstream) = duplex(PIPE);
        let mut bare_sender = FramedLink::new(upstream);
        let mut bare_receiver = FramedLink::new(downstream);
        for _index in 0..SAMPLES {
            let at = Instant::now();
            bare_sender
                .send(CHANNEL, b"k")
                .await
                .expect("a keystroke goes");
            let frame = bare_receiver
                .next_frame()
                .await
                .expect("it arrives")
                .expect("the link is open");
            assert_eq!(frame.payload, b"k", "a keystroke");
            plain_times.push(at.elapsed());
        }

        compressed_times.sort_unstable();
        plain_times.sort_unstable();
        let added = percentile(&compressed_times).saturating_sub(percentile(&plain_times));
        assert!(
            added <= SMALL_FRAME_LATENCY_CEILING,
            "compression added {added:?} to a one-byte frame at the ninety-ninth percentile, \
             over the {SMALL_FRAME_LATENCY_CEILING:?} ceiling; compressed median {:?} and \
             ninety-ninth {:?}, plain median {:?} and ninety-ninth {:?}",
            median(&compressed_times),
            percentile(&compressed_times),
            median(&plain_times),
            percentile(&plain_times)
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the latency case finishes");
}
