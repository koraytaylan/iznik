//! The scenario driver that runs inside a container and reports NDJSON, and the test binary that makes every scenario a nextest test.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod step;

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, a flag it cannot parse, or a subcommand whose task has not
/// landed yet. Two, the conventional usage-error status, so that a failed run's
/// one and a refused command line are told apart.
pub const USAGE_EXIT_CODE: u8 = 2;

/// The flag every subcommand answers with the line that says what it takes.
/// A person who has read a command's name in a README must be able to ask the
/// command what it wants without running it — and `step --help` in particular
/// must answer rather than wait on standard input for a step that is not
/// coming.
pub const HELP_FLAG: &str = "--help";

/// Whether a command line asks what a subcommand takes rather than asking it
/// to do the work.
#[must_use]
pub fn asked_for_help(arguments: &[std::ffi::OsString]) -> bool {
    arguments.iter().any(|argument| argument == HELP_FLAG)
}

/// A subcommand's usage line on standard output, and success.
///
/// That is the whole difference between asking and erring: the same line goes
/// to standard error with [`USAGE_EXIT_CODE`] when nobody asked for it and the
/// command line cannot be acted on.
#[must_use]
pub fn help_with(usage: &str) -> std::process::ExitCode {
    use std::io::Write as _;
    let _written = writeln!(std::io::stdout(), "{usage}");
    std::process::ExitCode::SUCCESS
}
