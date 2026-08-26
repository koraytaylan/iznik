//! The remote daemon: pseudoterminal ownership, terminal mirrors, history, sessions, multiplexing and resume.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod connection;
pub mod daemon;
pub mod history;
pub mod multiplexer;
pub mod pane;
pub mod pty;
pub mod relay;
pub mod resume;
pub mod session;
pub mod terminal;

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, a flag it cannot parse, or a subcommand whose task has not
/// landed yet. Two, the conventional usage-error status, so that a failed run's
/// one and a refused command line are told apart.
pub const USAGE_EXIT_CODE: u8 = 2;
