//! The `xtask` binary: a dispatcher that looks at the first argument only and hands the whole command line to the module that owns the subcommand.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use xtask::{
    USAGE_EXIT_CODE, claims, distribution, doctor, gate, header, policy, regression, soak,
};

/// The subcommands this binary routes, in the order `--help` lists them.
const SUBCOMMANDS: &[&str] = &[
    "check",
    "gate",
    "doctor",
    "policy",
    "claims",
    "regression",
    "distribution",
    "header",
    "soak",
];

/// Routes by the first argument. The owning module receives every argument after
/// the program name, its subcommand first, and parses its own flags.
fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("check" | "gate") => gate::run(&arguments),
        Some("doctor") => doctor::run(&arguments),
        Some("policy") => policy::run(&arguments),
        Some("claims") => claims::run(&arguments),
        Some("regression") => regression::run(&arguments),
        Some("distribution") => distribution::run(&arguments),
        Some("header") => header::run(&arguments),
        Some("soak") => soak::run(&arguments),
        Some("--help") => help(),
        _ => usage(),
    }
}

/// The usage line: the program and every subcommand it knows.
fn usage_line() -> String {
    format!("usage: xtask <{}> [arguments]", SUBCOMMANDS.join(" | "))
}

/// `--help`: the usage line on standard output, and success.
fn help() -> ExitCode {
    writeln!(std::io::stdout(), "{}", usage_line()).unwrap_or_default();
    ExitCode::SUCCESS
}

/// A first argument the dispatcher does not know, or none: the usage line on
/// standard error, and the usage exit code.
fn usage() -> ExitCode {
    writeln!(std::io::stderr(), "{}", usage_line()).unwrap_or_default();
    ExitCode::from(USAGE_EXIT_CODE)
}
