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

/// The flag every entry point answers with the line that says what it takes.
/// A person who has read a command's name in a README must be able to ask the
/// command what it wants without running it — and `--stdio --help` in
/// particular must say so rather than starting a daemon.
pub const HELP_FLAG: &str = "--help";

/// Whether a command line asks what an entry point takes rather than asking it
/// to do the work.
///
/// Anywhere in the line: a person who reaches for the flag after the
/// subcommand should not have to reach for it again before.
#[must_use]
pub fn asked_for_help(arguments: &[std::ffi::OsString]) -> bool {
    arguments.iter().any(|argument| argument == HELP_FLAG)
}

/// An entry point's usage line on standard output, and success.
///
/// That is the whole difference between asking and erring: the same line goes
/// to standard error with [`USAGE_EXIT_CODE`] when nobody asked for it and the
/// command line cannot be acted on. Through the runtime, because section 3.6
/// forbids this crate the standard library's blocking streams.
pub async fn help_with(usage: &str) -> std::process::ExitCode {
    use tokio::io::AsyncWriteExt;
    let mut stdout = tokio::io::stdout();
    let _written = stdout.write_all(format!("{usage}\n").as_bytes()).await;
    let _flushed = stdout.flush().await;
    std::process::ExitCode::SUCCESS
}
