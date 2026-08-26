//! `xtask regression images`, `stage` and `reap`: the container images, the staging directory, and the removal of every labelled container.
//!
//! Filled by task `regression-images` of plan 0001; until then this module holds only its documentation and the stub of its entry point.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// The subcommand's entry point: a stub until task `regression-images` replaces its body.
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
        "{subcommand}: not implemented until task regression-images"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
