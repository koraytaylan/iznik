//! The frame codec against its golden fixture: every line encodes and
//! decodes exactly, the decoder resumes across any split, refuses an
//! oversize header without allocating, asks for more bytes rather than
//! guessing, and keeps its buffer bounded over a long stream.

use std::error::Error;
use std::path::{Path, PathBuf};

use iznik_protocol::frame::{
    FrameDecoder, FrameError, FrameHeader, HEADER_LENGTH, MAXIMUM_PAYLOAD_LENGTH, encode,
};
use iznik_testkit::golden;

/// The golden fixture, relative to this crate.
const FIXTURE: &str = "tests/fixtures/frame.jsonl";

/// The most bytes a fixture payload may have to take part in the split
/// tests, which try every boundary and so want a short stream.
const SPLIT_PAYLOAD_LIMIT: usize = 512;

/// The one refusal a fixture line may declare.
const OVERSIZE: &str = "oversize";

/// One fixture line.
#[derive(Debug)]
struct Case {
    /// What the line says it is.
    description: String,
    /// The channel.
    channel: u8,
    /// The payload bytes.
    payload: Vec<u8>,
    /// The framed bytes.
    frame: Vec<u8>,
    /// Whether the line is one both directions must refuse as oversize.
    oversize: bool,
}

/// A decoded frame, owned.
type Decoded = (u8, Vec<u8>);

/// The string a fixture field holds.
///
/// # Errors
///
/// When the field is absent or not a string.
fn string_field(value: &serde_json::Value, field: &str) -> Result<String, Box<dyn Error>> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("a fixture line lacks the string field `{field}`").into())
}

/// The fixture's cases, in order.
///
/// # Errors
///
/// When the fixture cannot be loaded or a line is not shaped as one case.
fn cases() -> Result<Vec<Case>, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    golden::lines(&path)?
        .iter()
        .map(|value| {
            let channel = value
                .get("channel")
                .and_then(serde_json::Value::as_u64)
                .and_then(|number| u8::try_from(number).ok())
                .ok_or("a fixture line lacks a channel byte")?;
            let oversize = match value.get("error").map(serde_json::Value::as_str) {
                None => false,
                Some(Some(OVERSIZE)) => true,
                Some(other) => return Err(format!("unknown fixture error {other:?}").into()),
            };
            Ok(Case {
                description: string_field(value, "description")?,
                channel,
                payload: golden::bytes(&string_field(value, "payload_hex")?)?,
                frame: golden::bytes(&string_field(value, "frame_hex")?)?,
                oversize,
            })
        })
        .collect()
}

/// Whether two byte strings are the same, or where they first differ, with
/// both lengths: a megabyte of hex is not a message.
///
/// # Errors
///
/// The first differing offset and both lengths.
fn same_bytes(actual: &[u8], expected: &[u8]) -> Result<(), String> {
    let offset = actual
        .iter()
        .zip(expected)
        .position(|(left, right)| left != right)
        .or_else(|| (actual.len() != expected.len()).then_some(actual.len().min(expected.len())));
    match offset {
        None => Ok(()),
        Some(offset) => Err(format!(
            "bytes differ from offset {offset}: {} actual bytes, {} expected",
            actual.len(),
            expected.len()
        )),
    }
}

/// The next frame, owned, or `None`.
///
/// # Errors
///
/// The decoder's refusal.
fn drain_one(decoder: &mut FrameDecoder) -> Result<Option<Decoded>, FrameError> {
    Ok(decoder
        .next_frame()?
        .map(|frame| (frame.channel, frame.payload.to_vec())))
}

/// Every frame the decoder can yield now, owned.
///
/// # Errors
///
/// The decoder's error.
fn drain(decoder: &mut FrameDecoder) -> Result<Vec<Decoded>, FrameError> {
    let mut frames = Vec::new();
    while let Some(frame) = drain_one(decoder)? {
        frames.push(frame);
    }
    Ok(frames)
}

/// The cases short enough for the split tests, as one stream with the frames
/// it should yield.
///
/// # Errors
///
/// When the fixture cannot be loaded.
fn short_stream() -> Result<(Vec<u8>, Vec<Decoded>), Box<dyn Error>> {
    let mut stream = Vec::new();
    let mut expected = Vec::new();
    for case in cases()? {
        if case.oversize || case.payload.len() > SPLIT_PAYLOAD_LIMIT {
            continue;
        }
        stream.extend_from_slice(&case.frame);
        expected.push((case.channel, case.payload));
    }
    Ok((stream, expected))
}

/// Every fixture line is the contract in both directions.
///
/// # Panics
///
/// When a fixture line does not encode to its `frame_hex`, or decode to its
/// channel and `payload_hex`, or when an oversize line is not refused by
/// both directions; the message names the line's description.
#[test]
fn frame_golden_every_line_encodes_and_decodes_exactly() {
    let cases = cases().expect("the fixture loads");
    assert!(
        cases.iter().any(|case| case.oversize),
        "the fixture has an oversize line"
    );
    for case in &cases {
        let mut encoded = Vec::new();
        let outcome = encode(case.channel, &case.payload, &mut encoded);
        let mut decoder = FrameDecoder::new();
        decoder.push(&case.frame);
        if case.oversize {
            let length = u32::try_from(case.payload.len()).expect("the payload's length fits");
            assert_eq!(
                outcome,
                Err(FrameError::Oversize { length }),
                "{}",
                case.description
            );
            assert!(
                encoded.is_empty(),
                "{}: nothing is appended on refusal",
                case.description
            );
            assert_eq!(
                decoder.next_frame(),
                Err(FrameError::Oversize { length }),
                "{}",
                case.description
            );
            continue;
        }
        assert_eq!(outcome, Ok(()), "{}", case.description);
        if let Err(difference) = same_bytes(&encoded, &case.frame) {
            panic!("{}: encoded {difference}", case.description);
        }
        let frames = drain(&mut decoder).expect("the frame decodes");
        assert_eq!(frames.len(), 1, "{}: exactly one frame", case.description);
        let (channel, payload) = frames.first().expect("one frame");
        assert_eq!(*channel, case.channel, "{}", case.description);
        if let Err(difference) = same_bytes(payload, &case.payload) {
            panic!("{}: decoded payload {difference}", case.description);
        }
    }
}

/// The decoder resumes correctly across any read boundary.
///
/// # Panics
///
/// When any split of the short stream into two pushes yields frames other
/// than the ones the whole stream yields.
#[test]
fn frame_golden_resumes_across_every_split_into_two_pushes() {
    let (stream, expected) = short_stream().expect("the fixture loads");
    assert!(
        expected.len() > 1,
        "the short stream carries several frames"
    );
    for split in 0..=stream.len() {
        let (first, second) = stream.split_at(split);
        let mut decoder = FrameDecoder::new();
        decoder.push(first);
        let mut frames = drain(&mut decoder).expect("the first half decodes");
        decoder.push(second);
        frames.extend(drain(&mut decoder).expect("the second half decodes"));
        assert_eq!(frames, expected, "split at byte {split}");
    }
}

/// The decoder resumes correctly when every read is one byte.
///
/// # Panics
///
/// When pushing the short stream one byte at a time yields frames other than
/// the ones the whole stream yields.
#[test]
fn frame_golden_resumes_across_one_byte_pushes() {
    let (stream, expected) = short_stream().expect("the fixture loads");
    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    for byte in &stream {
        decoder.push(std::slice::from_ref(byte));
        frames.extend(drain(&mut decoder).expect("a byte at a time decodes"));
    }
    assert_eq!(frames, expected);
}

/// An oversize length is a protocol error, not an allocation.
///
/// # Panics
///
/// When an oversize header is not refused with its length, is refused only
/// once, or makes the decoder reserve room for the payload it names.
#[test]
fn frame_golden_refuses_an_oversize_header_without_allocating() {
    let length = MAXIMUM_PAYLOAD_LENGTH
        .checked_add(1)
        .expect("one more fits a u32");
    let mut header = Vec::new();
    FrameHeader { length, channel: 3 }.write(&mut header);
    assert_eq!(header.len(), HEADER_LENGTH);
    let mut decoder = FrameDecoder::new();
    decoder.push(&header);
    assert_eq!(decoder.next_frame(), Err(FrameError::Oversize { length }));
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::Oversize { length }),
        "the error persists"
    );
    let maximum = usize::try_from(MAXIMUM_PAYLOAD_LENGTH).expect("the maximum fits");
    assert!(
        decoder.capacity() < maximum,
        "the decoder reserved {} bytes for a payload it refused",
        decoder.capacity()
    );
}

/// The decoder asks for more bytes rather than guessing.
///
/// # Panics
///
/// When a partial header or a partial payload yields anything but "more
/// bytes", or the frame does not appear once the rest arrives.
#[test]
fn frame_golden_asks_for_more_bytes_until_a_frame_is_whole() {
    let payload = b"a whole frame";
    let mut frame = Vec::new();
    encode(5, payload, &mut frame).expect("a small payload encodes");
    let (header, rest) = frame.split_at(HEADER_LENGTH);
    let (header_start, header_end) = header.split_at(2);
    let (payload_start, payload_end) = rest.split_at(4);
    let mut decoder = FrameDecoder::new();
    decoder.push(header_start);
    assert_eq!(decoder.next_frame(), Ok(None), "a partial header");
    decoder.push(header_end);
    assert_eq!(
        decoder.next_frame(),
        Ok(None),
        "a header without its payload"
    );
    decoder.push(payload_start);
    assert_eq!(decoder.next_frame(), Ok(None), "a partial payload");
    decoder.push(payload_end);
    let frames = drain(&mut decoder).expect("the whole frame decodes");
    assert_eq!(frames, vec![(5, payload.to_vec())]);
    assert_eq!(decoder.next_frame(), Ok(None), "nothing remains");
}

/// The decoder's buffer stays bounded over a long stream.
///
/// # Panics
///
/// When a long stream of frames, pushed in chunks that straddle frame
/// boundaries, leaves the decoder holding a buffer that grew with the stream
/// rather than staying within a small multiple of the compaction threshold,
/// or when a frame goes missing along the way.
#[test]
fn frame_golden_bounds_its_buffer_over_a_long_stream() {
    let frame_count = 20_000;
    let payload: Vec<u8> = (0..100)
        .map(|index| u8::try_from(index).expect("fits"))
        .collect();
    let mut stream = Vec::new();
    for _ in 0..frame_count {
        encode(1, &payload, &mut stream).expect("a small payload encodes");
    }
    let chunk = 7_777;
    let bound = 256 * 1024;
    let mut decoder = FrameDecoder::new();
    let mut yielded: usize = 0;
    for piece in stream.chunks(chunk) {
        decoder.push(piece);
        yielded = yielded.saturating_add(drain(&mut decoder).expect("the stream decodes").len());
        assert!(
            decoder.capacity() <= bound,
            "the buffer grew to {} bytes after {yielded} frames",
            decoder.capacity()
        );
    }
    assert_eq!(yielded, frame_count);
}

/// A complete frame is yielded where it landed: within one push the decoder
/// never moves a payload.
///
/// # Panics
///
/// When two frames yielded from the same push do not sit at increasing
/// addresses, which a move would break, or a frame goes missing.
#[test]
fn frame_golden_never_moves_a_complete_frame() {
    let frame_count = 2_000;
    let payload = [0x5a; 100];
    let mut stream = Vec::new();
    for _ in 0..frame_count {
        encode(4, &payload, &mut stream).expect("a small payload encodes");
    }
    let mut decoder = FrameDecoder::new();
    decoder.push(&stream);
    let mut previous = None;
    let mut yielded: usize = 0;
    while let Some(frame) = decoder.next_frame().expect("the stream decodes") {
        let address = frame.payload.as_ptr().addr();
        if let Some(previous) = previous {
            assert!(
                address > previous,
                "frame {yielded} moved from above {previous:#x} to {address:#x}"
            );
        }
        previous = Some(address);
        yielded = yielded.saturating_add(1);
    }
    assert_eq!(yielded, frame_count);
}

/// `ready` and `pending` describe what the decoder holds without yielding
/// or moving anything.
///
/// # Panics
///
/// When `ready` is true before a frame is whole or false once it is, or
/// when `pending` is not exactly the bytes pushed and not yet yielded.
#[test]
fn frame_golden_ready_and_pending_track_a_frame_as_it_arrives() {
    let mut first = Vec::new();
    encode(2, b"first", &mut first).expect("a small payload encodes");
    let mut second = Vec::new();
    encode(3, b"second", &mut second).expect("a small payload encodes");
    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.ready(), Ok(false), "empty");
    assert!(decoder.pending().is_empty(), "empty");
    let (header_start, rest) = first.split_at(3);
    decoder.push(header_start);
    assert_eq!(decoder.ready(), Ok(false), "a partial header");
    assert_eq!(decoder.pending(), header_start);
    let (payload_start, payload_end) = rest.split_at(4);
    decoder.push(payload_start);
    assert_eq!(decoder.ready(), Ok(false), "a partial payload");
    assert_eq!(decoder.pending(), [header_start, payload_start].concat());
    decoder.push(payload_end);
    decoder.push(&second);
    assert_eq!(decoder.ready(), Ok(true), "a whole frame, and more");
    assert_eq!(
        decoder.pending(),
        [first.as_slice(), second.as_slice()].concat()
    );
    let yielded = drain_one(&mut decoder).expect("decodes");
    assert_eq!(yielded, Some((2, b"first".to_vec())));
    assert_eq!(
        decoder.pending(),
        second.as_slice(),
        "the second frame is what remains"
    );
    assert_eq!(decoder.ready(), Ok(true));
    assert_eq!(
        drain_one(&mut decoder).expect("decodes"),
        Some((3, b"second".to_vec()))
    );
    assert!(decoder.pending().is_empty());
    assert_eq!(decoder.ready(), Ok(false));
}

/// An empty payload's frame is whole at its five header bytes.
///
/// # Panics
///
/// When `ready` does not say so, or the frame does not yield empty.
#[test]
fn frame_golden_ready_holds_for_an_empty_payload() {
    let mut empty = Vec::new();
    encode(4, b"", &mut empty).expect("an empty payload encodes");
    let mut decoder = FrameDecoder::new();
    decoder.push(&empty);
    assert_eq!(decoder.ready(), Ok(true));
    assert_eq!(
        drain_one(&mut decoder).expect("decodes"),
        Some((4, Vec::new()))
    );
    assert_eq!(decoder.ready(), Ok(false));
}

/// `ready` refuses an oversize header exactly as `next_frame` would.
///
/// # Panics
///
/// When an oversize header is not refused by `ready`.
#[test]
fn frame_golden_ready_refuses_an_oversize_header() {
    let length = MAXIMUM_PAYLOAD_LENGTH
        .checked_add(1)
        .expect("one more fits a u32");
    let mut oversize = Vec::new();
    FrameHeader { length, channel: 1 }.write(&mut oversize);
    let mut decoder = FrameDecoder::new();
    decoder.push(&oversize);
    assert_eq!(decoder.ready(), Err(FrameError::Oversize { length }));
    assert_eq!(
        decoder.pending(),
        oversize.as_slice(),
        "nothing was consumed"
    );
    assert_eq!(decoder.next_frame(), Err(FrameError::Oversize { length }));
}

/// A link's loop — ask `ready`, push until it says so, yield — keeps the
/// buffer bounded too: `ready` compacts when it asks for more bytes.
///
/// # Panics
///
/// When the buffer grows with the stream, when `ready` says a frame waits
/// and none is yielded, or when a frame goes missing.
#[test]
fn frame_golden_bounds_its_buffer_through_a_link_loop() {
    let frame_count = 20_000;
    let payload = [0x3c; 100];
    let mut stream = Vec::new();
    for _ in 0..frame_count {
        encode(1, &payload, &mut stream).expect("a small payload encodes");
    }
    let chunk = 7_777;
    let bound = 256 * 1024;
    let mut decoder = FrameDecoder::new();
    let mut yielded: usize = 0;
    for piece in stream.chunks(chunk) {
        decoder.push(piece);
        while decoder.ready().expect("the stream decodes") {
            assert!(
                drain_one(&mut decoder).expect("decodes").is_some(),
                "ready means a frame"
            );
            yielded = yielded.saturating_add(1);
        }
        assert!(
            decoder.capacity() <= bound,
            "the buffer grew to {} bytes after {yielded} frames",
            decoder.capacity()
        );
    }
    assert_eq!(yielded, frame_count);
}

/// A scratch file removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_file(&self.0);
    }
}

/// A malformed golden is reported by file and line.
///
/// # Panics
///
/// When a JSONL file whose second line is not JSON loads, or when the error
/// does not name the file and that line.
#[test]
fn frame_golden_loader_names_the_file_and_line_of_a_malformed_case() {
    let path = std::env::temp_dir().join(format!(
        "iznik-frame-golden-{}-malformed.jsonl",
        std::process::id()
    ));
    let scratch = Scratch(path.clone());
    std::fs::write(&scratch.0, "{\"description\": \"fine\"}\n{not json\n").expect("writes");
    let error = golden::lines(&path).expect_err("a malformed line fails the load");
    let message = error.to_string();
    let expected_prefix = format!("{}:2:", path.display());
    assert!(
        message.starts_with(&expected_prefix),
        "`{message}` does not begin with `{expected_prefix}`"
    );
    assert!(
        matches!(error, golden::GoldenError::Parse { line: 2, .. }),
        "{error:?}"
    );
}

/// The hex helpers are strict and round-trip.
///
/// # Panics
///
/// When the hex helpers accept an odd number of digits or a non-digit, or do
/// not round-trip bytes in lowercase.
#[test]
fn frame_golden_hex_round_trips_and_rejects_what_is_not_hex() {
    assert_eq!(golden::hex(&[0xab, 0x01, 0xff]), "ab01ff");
    assert_eq!(
        golden::bytes("AB01ff").expect("uppercase decodes"),
        vec![0xab, 0x01, 0xff]
    );
    assert_eq!(golden::bytes("").expect("empty decodes"), Vec::<u8>::new());
    assert!(golden::bytes("abc").is_err(), "an odd number of digits");
    assert!(golden::bytes("zz").is_err(), "not a digit");
    assert!(golden::bytes("+f").is_err(), "a sign is not a digit");
}
