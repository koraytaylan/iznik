//! `xtask distribution --target <triple>`: reproducible release artifacts with checksums and a manifest.
//!
//! Filled by task `linux-artifacts` of plan 0004; until then this module holds only its documentation and the stub of its entry point.

pub mod darwin;
pub mod linux;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// The subcommand's entry point: a stub until task `linux-artifacts` replaces its body.
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
        "{subcommand}: not implemented until task linux-artifacts"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
