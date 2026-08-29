//! The `iznik` binary: a dispatcher that looks at the first argument only and hands the whole command line to the module that owns the subcommand.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use iznik_cli::{
    HELP_FLAG, USAGE_EXIT_CODE, benchmark, doctor, help_with, probe, state, tail, uninstall,
};

/// The subcommands this binary routes, in the order `--help` lists them.
const SUBCOMMANDS: &[&str] = &["probe", "state", "tail", "benchmark", "doctor", "uninstall"];

/// Routes by the first argument. The owning module receives every argument after
/// the program name, its subcommand first, and parses its own flags.
fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("probe") => probe::run(&arguments),
        Some("state") => state::run(&arguments),
        Some("tail") => tail::run(&arguments),
        Some("benchmark") => benchmark::run(&arguments),
        Some("doctor") => doctor::run(&arguments),
        Some("uninstall") => uninstall::run(&arguments),
        Some(named) if named == HELP_FLAG => help(),
        _ => usage(),
    }
}

/// The usage line: the program and every subcommand it knows.
fn usage_line() -> String {
    format!("usage: iznik <{}> [arguments]", SUBCOMMANDS.join(" | "))
}

/// `--help`: the usage line on standard output, and success, through the
/// same writer every subcommand answers with.
fn help() -> ExitCode {
    help_with(&usage_line())
}

/// A first argument the dispatcher does not know, or none: the usage line on
/// standard error, and the usage exit code.
fn usage() -> ExitCode {
    writeln!(std::io::stderr(), "{}", usage_line()).unwrap_or_default();
    ExitCode::from(USAGE_EXIT_CODE)
}
