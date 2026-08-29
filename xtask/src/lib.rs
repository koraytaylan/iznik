//! The gates, the policy checks, the claims registry, the images, the staging, the distribution, the header and the soak behind `cargo xtask`.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub mod claims;
pub mod distribution;
pub mod doctor;
pub mod gate;
pub mod header;
pub mod policy;
pub mod regression;
pub mod soak;

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, or a flag it cannot parse. Two, the conventional usage-error status, so that a failed run's
/// one and a refused command line are told apart.
pub const USAGE_EXIT_CODE: u8 = 2;

/// The flag every subcommand answers with the line that says what it takes.
/// A person who has read a command's name in a README must be able to ask the
/// command what it wants without running it, and a README that names a
/// command no binary has is a document that has drifted; `readme_commands`
/// asks every one of them.
pub const HELP_FLAG: &str = "--help";

/// Whether a command line asks what a command takes rather than asking it to
/// do the work.
///
/// Anywhere in the line: `xtask claims verify --help` is the same question as
/// `xtask claims --help`, and a person who reaches for the flag late should
/// not have to reach for it again earlier.
#[must_use]
pub fn asked_for_help(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| argument == HELP_FLAG)
}

/// A command's usage line on standard output, and success.
///
/// That is the whole difference between asking and erring: the same line goes
/// to standard error with [`USAGE_EXIT_CODE`] when nobody asked for it and the
/// command line cannot be acted on.
#[must_use]
pub fn help_with(usage: &str) -> ExitCode {
    let _written = writeln!(std::io::stdout(), "{usage}");
    ExitCode::SUCCESS
}

/// The repository root.
///
/// `xtask` is only ever run through `cargo xtask` from inside this repository,
/// so the root is the parent of this crate's directory, known when the crate
/// is compiled; it is not derived from the current directory, which is
/// wherever the person invoking cargo happens to be.
#[must_use]
pub fn repository_root() -> PathBuf {
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_directory
        .parent()
        .map_or_else(|| manifest_directory.to_path_buf(), Path::to_path_buf)
}
