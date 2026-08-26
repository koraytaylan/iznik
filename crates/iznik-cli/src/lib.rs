//! Developer plumbing: `probe`, `state`, `tail`, `benchmark`, `doctor` and `uninstall`, printing structured text and never drawing a screen.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod benchmark;
pub mod doctor;
pub mod output;
pub mod probe;
pub mod state;
pub mod tail;
pub mod uninstall;

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, a flag it cannot parse, or a subcommand whose task has not
/// landed yet. Two, the conventional usage-error status, so that a failed run's
/// one and a refused command line are told apart.
pub const USAGE_EXIT_CODE: u8 = 2;
