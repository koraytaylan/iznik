//! `xtask policy`: the policy checks, each a pure function from a repository root to a list of violations.
//!
//! Filled by task `policy-gates` of plan 0001; until then this module holds only its documentation and the stub of its entry point.

pub mod attributes;
pub mod blocking;
pub mod dependencies;
pub mod documentation;
pub mod length;
pub mod lexicon;
pub mod links;
pub mod literals;
pub mod unsafe_boundary;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// The subcommand's entry point: a stub until task `policy-gates` replaces its body.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let subcommand = arguments.first().map_or_else(String::new, |argument| {
        argument.to_string_lossy().into_owned()
    });
    writeln!(
        std::io::stderr(),
        "{subcommand}: not implemented until task policy-gates"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
