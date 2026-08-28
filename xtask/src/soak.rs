//! `xtask soak`: hours of the end-to-end stack against two fixture hosts with faults, sampling memory and losing no byte.
//!
//! Filled by task `soak-and-release` of plan 0006; until then this module holds only its documentation and the stub of its entry point.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// What this subcommand will take. It answers now, ahead of the work,
/// because a README names it and a named command that cannot say what it
/// is has already drifted from the document.
const USAGE: &str = "usage: xtask soak (not implemented until task soak-and-release)";

/// The subcommand's entry point: a stub until task `soak-and-release` replaces its body.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    let subcommand = arguments.first().map_or_else(String::new, |argument| {
        argument.to_string_lossy().into_owned()
    });
    writeln!(
        std::io::stderr(),
        "{subcommand}: not implemented until task soak-and-release"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
