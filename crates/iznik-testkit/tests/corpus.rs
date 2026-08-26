//! The fidelity corpus against its golden: the builder serializes to
//! exactly the committed bytes, every construct is named and the named ones
//! are present, the boundaries that matter fall inside sequences, and the
//! generator is deterministic, printable and fast.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use iznik_testkit::corpus::{Construct, CorpusError, constructs, generated, parse, serialize};

/// The golden, relative to this crate.
const GOLDEN: &str = "assets/fidelity-corpus.bin";

/// The inventory's flood: 64 MiB.
const FLOOD_BYTES: usize = 64 * 1024 * 1024;

/// The inventory's ceiling on producing the flood.
const FLOOD_CEILING: Duration = Duration::from_secs(1);

/// The escape byte no generated text may contain.
const ESCAPE: u8 = 0x1b;

/// The committed golden's bytes.
///
/// # Errors
///
/// When the golden cannot be read.
fn golden() -> Result<Vec<u8>, std::io::Error> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN))
}

/// The name of the first construct that differs between two lists, or of
/// the first missing one.
fn first_difference(built: &[Construct], committed: &[Construct]) -> Option<String> {
    for (index, construct) in built.iter().enumerate() {
        match committed.get(index) {
            Some(golden) if golden == construct => {}
            Some(golden) if golden.name == construct.name => {
                return Some(format!("{} (its chunks differ)", construct.name));
            }
            Some(golden) => return Some(format!("{} (golden: {})", construct.name, golden.name)),
            None => return Some(format!("{} (absent from the golden)", construct.name)),
        }
    }
    committed
        .get(built.len())
        .map(|extra| format!("{} (absent from the builder)", extra.name))
}

/// Serializing `constructs()` yields exactly the committed bytes.
///
/// # Panics
///
/// When they differ; the message names the first construct that does.
#[test]
fn corpus_golden_is_exactly_what_the_builder_serializes() {
    let committed = golden().expect("the golden is read");
    let parsed = parse(&committed).expect("the golden parses");
    let built = constructs();
    if let Some(name) = first_difference(&built, &parsed) {
        panic!("the first construct that differs from the golden: {name}");
    }
    assert_eq!(
        serialize(&built),
        committed,
        "the bytes differ though the constructs agree"
    );
}

/// Every construct has a distinct name, and the named ones are all present.
///
/// # Panics
///
/// When a name repeats or a required construct is absent.
#[test]
fn corpus_names_are_distinct_and_the_named_ones_present() {
    let built = constructs();
    let names: HashSet<&str> = built
        .iter()
        .map(|construct| construct.name.as_str())
        .collect();
    assert_eq!(names.len(), built.len(), "a name repeats");
    for required in [
        "kitty graphics",
        "hyperlink",
        "clipboard",
        "mark prompt start",
        "mark command start",
        "mark command executed",
        "mark command finished",
        "working directory",
        "title terminated by BEL",
        "title terminated by ST",
        "CSI split",
        "wide CJK glyph",
        "lone escape",
        "synchronized output",
        "alternate screen",
        "keyboard protocol query",
        "cursor position query",
    ] {
        assert!(
            names.iter().any(|name| name.contains(required)),
            "no construct named for `{required}`"
        );
    }
}

/// The split CSI and the lone escape are constructs of two chunks whose
/// boundary falls inside a sequence.
///
/// # Panics
///
/// When either has other than two chunks, its first chunk ends outside a
/// sequence, or its second chunk does not complete it.
#[test]
fn corpus_chunk_boundaries_fall_inside_sequences() {
    let built = constructs();
    let find = |part: &str| {
        built
            .iter()
            .find(|construct| construct.name.contains(part))
            .unwrap_or_else(|| panic!("no construct named for `{part}`"))
    };
    let split = find("CSI split");
    assert_eq!(split.chunks.len(), 2);
    let first = &split.chunks[0];
    assert!(first.starts_with(b"\x1b["), "the first chunk opens the CSI");
    assert!(
        !first
            .last()
            .is_some_and(|byte| (0x40..=0x7e).contains(byte)),
        "the first chunk must not end with a CSI final byte"
    );
    let completing = split
        .chunks
        .get(1)
        .and_then(|chunk| chunk.iter().find(|byte| !(0x30..=0x3f).contains(*byte)));
    assert!(
        completing.is_some_and(|byte| (0x40..=0x7e).contains(byte)),
        "the first byte of the second chunk past the parameters must be the CSI's final byte"
    );
    let lone = find("lone escape");
    assert_eq!(lone.chunks.len(), 2);
    assert_eq!(
        lone.chunks[0].last(),
        Some(&ESCAPE),
        "the first chunk ends with the escape"
    );
    assert!(
        lone.chunks[1].starts_with(b"["),
        "the second chunk continues the sequence"
    );
}

/// `generated` is deterministic, differs by seed, and is printable text
/// with line breaks and no escape byte.
///
/// # Panics
///
/// When two runs differ, two seeds agree, a byte is outside the run from
/// the space to the underscore and not a line break, no capital or no digit
/// occurs, or a zero seed generates only spaces.
#[test]
fn corpus_generated_text_is_deterministic_and_printable() {
    let first = generated(7, 100_000);
    let again = generated(7, 100_000);
    assert_eq!(first, again, "the same seed and length differ");
    assert_eq!(first.len(), 100_000);
    assert_ne!(first, generated(8, 100_000), "two seeds agree");
    assert!(first.contains(&b'\n'), "no line breaks");
    assert!(!first.contains(&ESCAPE), "an escape byte");
    assert!(
        first
            .iter()
            .all(|byte| *byte == b'\n' || (0x20..=0x5f).contains(byte)),
        "a byte that is neither in the space-to-underscore run nor a line break"
    );
    assert!(
        first.iter().any(u8::is_ascii_uppercase),
        "no capital letter"
    );
    assert!(first.iter().any(u8::is_ascii_digit), "no digit");
    let zero_seeded = generated(0, 16);
    assert_eq!(zero_seeded.len(), 16);
    assert!(
        zero_seeded.iter().any(|byte| *byte != b' '),
        "a zero seed generates more than spaces"
    );
}

/// `parse` refuses bytes that end inside a word or a body, naming where the
/// cut thing began, and a name that is not UTF-8.
///
/// # Panics
///
/// When a cut or a bad name is not refused with the documented offset.
#[test]
fn corpus_parse_refuses_a_cut_or_a_bad_name_by_offset() {
    let committed = golden().expect("the golden is read");
    assert_eq!(parse(&[]).expect("nothing parses"), Vec::new());
    assert_eq!(
        parse(&committed[..2]),
        Err(CorpusError::Truncated { offset: 0 })
    );
    assert_eq!(
        parse(&committed[..10]),
        Err(CorpusError::Truncated { offset: 4 })
    );
    let name_length = usize::try_from(u32::from_le_bytes([
        committed[0],
        committed[1],
        committed[2],
        committed[3],
    ]))
    .expect("fits");
    let count_offset = name_length.checked_add(4).expect("fits");
    let first_body = count_offset.checked_add(8).expect("fits");
    assert_eq!(
        parse(&committed[..=first_body]),
        Err(CorpusError::Truncated { offset: first_body })
    );
    let mut bad_name = committed.clone();
    bad_name[4] = 0xff;
    assert_eq!(parse(&bad_name), Err(CorpusError::Utf8 { offset: 0 }));
}

/// 64 MiB of generated text is produced in under a second; a baseline, so
/// nextest gives it the machine.
///
/// # Panics
///
/// When the flood takes the ceiling or more.
#[test]
fn corpus_flood_baseline_generates_64_mebibytes_in_under_a_second() {
    let started = Instant::now();
    let flood = generated(1, FLOOD_BYTES);
    let elapsed = started.elapsed();
    assert_eq!(flood.len(), FLOOD_BYTES);
    assert!(elapsed < FLOOD_CEILING, "the flood took {elapsed:?}");
}
