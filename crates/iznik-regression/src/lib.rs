//! The scenario driver that runs inside a container and reports NDJSON, and the test binary that makes every scenario a nextest test.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod step;

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, a flag it cannot parse, or a subcommand whose task has not
/// landed yet. Two, the conventional usage-error status, so that a failed run's
/// one and a refused command line are told apart.
pub const USAGE_EXIT_CODE: u8 = 2;
