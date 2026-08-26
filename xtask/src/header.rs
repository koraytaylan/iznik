//! `xtask header`: generating `include/iznik.h` with cbindgen, for the golden test that pins the ABI.
//!
//! Filled by task `static-library-and-header` of plan 0006; until then this module holds only its documentation and the stub of its entry point.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// The subcommand's entry point: a stub until task `static-library-and-header` replaces its body.
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
        "{subcommand}: not implemented until task static-library-and-header"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
