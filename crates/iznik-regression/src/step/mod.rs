//! The step dispatcher: one step read as TOML on standard input, executed in its own process group under its deadline, reported as one NDJSON record.
//!
//! Filled by task `scenario-driver` of plan 0001; until then this module holds only its documentation and the stub of its entry point.

pub mod bootstrap;
pub mod channel;
pub mod client;
pub mod manager;
pub mod pane;
pub mod probe;
pub mod run;
pub mod transport;
pub mod upload;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// The subcommand's entry point: a stub until task `scenario-driver` replaces its body.
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
        "{subcommand}: not implemented until task scenario-driver"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
