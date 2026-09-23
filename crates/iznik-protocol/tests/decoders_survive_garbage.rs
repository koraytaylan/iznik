//! Every decoder refuses garbage with an error value and never panics.
//!
//! A decoder reads bytes a peer sent, and a peer is whatever is at the other
//! end of an SSH channel: a daemon from another build, a truncated stream, a
//! bit that flipped. The goldens pin what well-formed bytes mean; this holds
//! every decoder — the frame codec, both message directions, the session
//! command and its outcome, the delta and the host model — to surviving
//! everything else. Every golden line is mutated many ways (bits flipped,
//! bytes overwritten, lengths set to the largest value, cut short, extended)
//! and random buffers are added; each result goes to every decoder.
//!
//! The generator is a hand-rolled xorshift, so a run is reproducible from
//! its seed. The seed is fixed, overridden by `IZNIK_DECODER_SEED`, and
//! printed with the input whenever a decoder panics, so the failure can be
//! replayed exactly.

use std::error::Error;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use iznik_protocol::command::{decode_command_outcome, decode_session_command};
use iznik_protocol::delta::decode_delta;
use iznik_protocol::frame::FrameDecoder;
use iznik_protocol::message::{decode_to_client, decode_to_server};
use iznik_protocol::model::decode_host_model;
use iznik_testkit::golden;

/// Every golden fixture, relative to this crate.
const FIXTURES: &[&str] = &[
    "tests/fixtures/command.jsonl",
    "tests/fixtures/delta.jsonl",
    "tests/fixtures/frame.jsonl",
    "tests/fixtures/message.jsonl",
    "tests/fixtures/model.jsonl",
];

/// What a fixture field holding bytes is named with.
const HEX_SUFFIX: &str = "hex";

/// The seed a run starts from unless [`SEED_VARIABLE`] names another.
const DEFAULT_SEED: u64 = 0x1D2A_4B9C_E5F0_7301;

/// The variable that overrides [`DEFAULT_SEED`], in decimal or `0x` hex.
const SEED_VARIABLE: &str = "IZNIK_DECODER_SEED";

/// The prefix of a hexadecimal seed.
const HEX_PREFIX: &str = "0x";

/// The radix of a hexadecimal seed.
const HEX_RADIX: u32 = 16;

/// How many mutations each golden input is put through.
const MUTATIONS_PER_INPUT: usize = 160;

/// The longest input whose every prefix is decoded. Two frame goldens carry
/// payloads of mebibytes, where every prefix would be a quadratic run and
/// every mutation a copy of the whole; those get [`LARGE_MUTATIONS`] instead,
/// cuts among them.
const EXHAUSTIVE_LENGTH: usize = 4096;

/// How many mutations an input longer than [`EXHAUSTIVE_LENGTH`] is put
/// through.
const LARGE_MUTATIONS: usize = 8;

/// How many random buffers are decoded beside the mutated goldens.
const RANDOM_BUFFERS: usize = 4000;

/// The longest random buffer.
const RANDOM_LENGTH: u64 = 96;

/// The most bytes one extension appends.
const EXTENSION_LENGTH: u64 = 24;

/// The most bits one mutation flips.
const FLIPS: u64 = 4;

/// Bits in a byte, for choosing one of them.
const BITS: u64 = 8;

/// How many kinds of mutation there are; see [`mutate`].
const KINDS: u64 = 5;

/// The mutation that flips bits.
const FLIP: u64 = 0;

/// The mutation that overwrites one byte.
const OVERWRITE: u64 = 1;

/// The mutation that sets a length-sized field to its largest value.
const LARGEST: u64 = 2;

/// The mutation that cuts the input short. Any other draw extends it.
const CUT: u64 = 3;

/// The width of the length and count fields a mutation overwrites with the
/// largest value.
const LENGTH_WIDTH: usize = 4;

/// The fewest cases a run must reach to count as tens of thousands.
const LEAST_CASES: usize = 20_000;

/// The xorshift triple, Marsaglia's `(13, 7, 17)` for 64 bits.
const FIRST_SHIFT: u32 = 13;

/// The second shift of the triple.
const SECOND_SHIFT: u32 = 7;

/// The third shift of the triple.
const THIRD_SHIFT: u32 = 17;

/// A reproducible stream of numbers.
struct Xorshift {
    /// The state; never zero, which is a fixed point.
    state: u64,
}

impl Xorshift {
    /// A generator starting from `seed`; a zero seed is moved off zero.
    fn new(seed: u64) -> Xorshift {
        Xorshift { state: seed | 1 }
    }

    /// The next number.
    fn next(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state.wrapping_shl(FIRST_SHIFT);
        state ^= state.wrapping_shr(SECOND_SHIFT);
        state ^= state.wrapping_shl(THIRD_SHIFT);
        self.state = state;
        state
    }

    /// A number below `bound`, or zero when `bound` is zero.
    fn below(&mut self, bound: u64) -> u64 {
        self.next().checked_rem(bound).unwrap_or(0)
    }

    /// An index below `length`, or zero when `length` is zero.
    fn index(&mut self, length: usize) -> usize {
        let bound = u64::try_from(length).unwrap_or(u64::MAX);
        usize::try_from(self.below(bound)).unwrap_or(0)
    }

    /// A byte.
    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes().first().copied().unwrap_or(0)
    }

    /// Up to `most` random bytes, at least one.
    fn bytes(&mut self, most: u64) -> Vec<u8> {
        let count = self.below(most).saturating_add(1);
        (0..count).map(|_byte| self.byte()).collect()
    }
}

/// The seed this run uses: [`SEED_VARIABLE`] when it is set and parses,
/// else [`DEFAULT_SEED`].
///
/// # Errors
///
/// When the variable is set and is not a number, so a mistyped seed is not
/// silently replaced by the default.
fn seed() -> Result<u64, Box<dyn Error>> {
    let Ok(written) = std::env::var(SEED_VARIABLE) else {
        return Ok(DEFAULT_SEED);
    };
    let parsed = match written.strip_prefix(HEX_PREFIX) {
        Some(digits) => u64::from_str_radix(digits, HEX_RADIX),
        None => written.parse(),
    };
    parsed.map_err(|error| format!("{SEED_VARIABLE}={written}: {error}").into())
}

/// Every byte string the goldens hold: each field whose name ends in
/// [`HEX_SUFFIX`], from every line of every fixture.
///
/// # Errors
///
/// When a fixture cannot be read or holds a field that is not hex.
fn golden_inputs() -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut inputs = Vec::new();
    for fixture in FIXTURES {
        for line in golden::lines(&root.join(Path::new(fixture)))? {
            let Some(fields) = line.as_object() else {
                continue;
            };
            for (name, value) in fields {
                if let (true, Some(hex)) = (name.ends_with(HEX_SUFFIX), value.as_str()) {
                    inputs.push(golden::bytes(hex)?);
                }
            }
        }
    }
    Ok(inputs)
}

/// One mutation of `input`, of a kind the generator picks: bits flipped, a
/// byte overwritten, a length-sized field set to its largest value, the input
/// cut short, or random bytes appended.
fn mutate(input: &[u8], generator: &mut Xorshift) -> Vec<u8> {
    let mut mutated = input.to_vec();
    match generator.below(KINDS) {
        FLIP => {
            for _flip in 0..generator.below(FLIPS).saturating_add(1) {
                let at = generator.index(mutated.len());
                let bit = generator.below(BITS);
                if let Some(byte) = mutated.get_mut(at) {
                    *byte ^= 1_u8.wrapping_shl(u32::try_from(bit).unwrap_or(0));
                }
            }
        }
        OVERWRITE => {
            let at = generator.index(mutated.len());
            let value = generator.byte();
            if let Some(byte) = mutated.get_mut(at) {
                *byte = value;
            }
        }
        LARGEST => {
            let at = generator.index(mutated.len());
            for byte in mutated.iter_mut().skip(at).take(LENGTH_WIDTH) {
                *byte = u8::MAX;
            }
        }
        CUT => mutated.truncate(generator.index(mutated.len())),
        _ => mutated.extend(generator.bytes(EXTENSION_LENGTH)),
    }
    mutated
}

/// Hands `bytes` to every decoder, discarding what each says: an error is
/// the right answer to garbage, and so is a value when the garbage happens
/// to be well formed. Only a panic is wrong.
fn decode_everything(bytes: &[u8]) {
    let _server = decode_to_server(bytes);
    let _client = decode_to_client(bytes);
    let _command = decode_session_command(bytes);
    let _outcome = decode_command_outcome(bytes);
    let _delta = decode_delta(bytes);
    let _model = decode_host_model(bytes);
    let mut frames = FrameDecoder::new();
    frames.push(bytes);
    while let Ok(Some(_frame)) = frames.next_frame() {}
}

/// Decodes `bytes` with every decoder, and panics — naming the seed, the case
/// and the input, so it can be replayed — if any of them panicked.
///
/// # Panics
///
/// When a decoder panics on `bytes`.
fn survives(seed: u64, case: usize, bytes: &[u8]) {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| decode_everything(bytes)));
    assert!(
        outcome.is_ok(),
        "a decoder panicked on case {case} with {SEED_VARIABLE}={seed:#x}; input {}",
        golden::hex(bytes)
    );
}

/// Every golden input, every prefix of it, many mutations of it, and random
/// buffers besides — tens of thousands of cases — pass through every decoder
/// without a panic.
///
/// # Panics
///
/// When a decoder panics, naming the seed and the input; or when the run is
/// smaller than [`LEAST_CASES`].
#[test]
fn decoders_survive_mutated_goldens_and_random_bytes() {
    let seed = seed().expect("a seed");
    let mut generator = Xorshift::new(seed);
    let inputs = golden_inputs().expect("the goldens");
    assert!(!inputs.is_empty(), "the goldens hold inputs");
    let mut case = 0_usize;
    for input in &inputs {
        survives(seed, case, input);
        let small = input.len() <= EXHAUSTIVE_LENGTH;
        let cuts = if small { input.len() } else { 0 };
        for cut in 0..cuts {
            case = case.saturating_add(1);
            survives(seed, case, &input[..cut]);
        }
        let mutations = if small {
            MUTATIONS_PER_INPUT
        } else {
            LARGE_MUTATIONS
        };
        for _mutation in 0..mutations {
            case = case.saturating_add(1);
            let mutated = mutate(input, &mut generator);
            survives(seed, case, &mutated);
            // A second mutation on top of the first reaches states one
            // mutation cannot, such as a flipped tag with a cut length.
            let twice = mutate(&mutated, &mut generator);
            survives(seed, case, &twice);
        }
    }
    for _buffer in 0..RANDOM_BUFFERS {
        case = case.saturating_add(1);
        let random = generator.bytes(RANDOM_LENGTH);
        survives(seed, case, &random);
    }
    assert!(
        case >= LEAST_CASES,
        "only {case} cases ran, fewer than {LEAST_CASES}"
    );
}

/// The generator is reproducible: one seed, one stream.
///
/// # Panics
///
/// When two generators from one seed disagree, or one from zero is stuck.
#[test]
fn decoders_survive_the_generator_is_reproducible() {
    let mut first = Xorshift::new(DEFAULT_SEED);
    let mut second = Xorshift::new(DEFAULT_SEED);
    for _draw in 0..1000 {
        assert_eq!(first.next(), second.next());
    }
    let mut zero = Xorshift::new(0);
    assert_ne!(zero.next(), zero.next(), "a zero seed still draws");
}
