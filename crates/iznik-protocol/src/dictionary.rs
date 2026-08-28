//! The committed zstd dictionary trained on the fidelity corpus, which primes
//! the compressed link from its first frame.
//!
//! A stream compressor learns its input as it goes, so the first kilobytes of
//! a connection are the expensive ones — and a terminal session's first
//! kilobytes are the ones a person is waiting for. A dictionary trained on the
//! constructs terminal output is actually made of gives the compressor that
//! learning before the first byte arrives.
//!
//! It is committed rather than trained at build time so that both ends of
//! every link hold the same bytes: a dictionary is part of the wire format,
//! and two peers that trained their own would not understand each other.
//!
//! # How it was trained
//!
//! From `iznik-testkit`'s `assets/fidelity-corpus.bin`, whose format that
//! crate's `corpus` module documents: one sample file per construct, holding
//! that construct's chunks concatenated and nothing else — not the names or
//! the length prefixes, which are the golden's framing and never reach a
//! wire. Then, with zstd 1.5.7:
//!
//! ```text
//! zstd --train <one file per construct> --maxdict=4096 \
//!      -o crates/iznik-protocol/assets/compression-dictionary.bin
//! ```
//!
//! The corpus is 316 bytes of payload across seventeen constructs, which is
//! far less than the hundredfold `zstd --train` asks for and it says so; the
//! dictionary it settles on is 378 bytes and `--maxdict` above 1024 does not
//! change it. What decides whether that is good enough is the measured ratio,
//! not the warning: `iznik-link`'s `compression` tests hold it to
//! `MINIMUM_CORPUS_RATIO`.
//!
//! The corpus these bytes were trained on has SHA-256
//! `13506351677ddc7b960964b8d8a5014c1c11bbefd1cddcf0089fceb4ece89673`. A
//! corpus that changes is a dictionary that must be retrained, and the ratio
//! test is what notices.

/// The dictionary both ends of a compressed link are primed with.
pub const COMPRESSION_DICTIONARY: &[u8] = include_bytes!("../assets/compression-dictionary.bin");
