//! `cargo app` (`xtask app`): build the servers the application installs on
//! ssh hosts, then run the application.
//!
//! A bundle carries its servers; a workspace build has what
//! `cargo xtask distribution` wrote into cargo's target directory, where the
//! application looks for them. This is that build and `cargo run` as one
//! command, with cargo's progress on the terminal for both, so a person never
//! has to know the servers are a separate step. Up to date, the server build
//! takes about a second; after a change to the server, it is rebuilt.

use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, ExitCode};

use iznik_harness::process::Output;

use crate::distribution::{build_with_output, target_directory, workspace_root};

/// The flag naming a server to build, repeatable.
const TARGET_FLAG: &str = "--target";

/// The usage line, shared by the refusal and `--help`.
const USAGE_LINE: &str = "usage: cargo app [--target <triple>]...";

/// The variable that tells cargo which target directory to use.
const TARGET_DIRECTORY_VARIABLE: &str = "CARGO_TARGET_DIR";

/// The server a host of this machine's own architecture runs, which is what
/// connections from a development machine most often need.
#[must_use]
pub fn native_server() -> String {
    format!("{}-unknown-linux-musl", std::env::consts::ARCH)
}

/// The servers asked for: every `--target`, or this machine's own when none
/// is given. `None` when the arguments are not understood.
#[must_use]
pub fn servers(arguments: &[OsString]) -> Option<Vec<String>> {
    let mut rest = arguments.iter().skip(1);
    let mut wanted = Vec::new();
    while let Some(argument) = rest.next() {
        if argument.to_str() != Some(TARGET_FLAG) {
            return None;
        }
        wanted.push(rest.next()?.to_str()?.to_owned());
    }
    if wanted.is_empty() {
        wanted.push(native_server());
    }
    Some(wanted)
}

/// The subcommand's entry point.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE_LINE);
    }
    let Some(servers) = servers(arguments) else {
        return refuse(USAGE_LINE);
    };
    let root = workspace_root();
    for triple in &servers {
        say(&format!(
            "cargo app: building iznik-server for {triple}, which is installed on ssh hosts"
        ));
        if let Err(error) = build_with_output(&root, triple, Output::Inherit) {
            return refuse(&format!("cargo app: {error}"));
        }
    }
    say("cargo app: starting iznik-app");
    // The application runs until a person closes it, so it has no deadline;
    // what it is given is the same target directory the servers went into,
    // two directories up from the executable cargo builds there.
    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(&root)
        .args(["run", "--locked", "--package", "iznik-app"])
        .env(TARGET_DIRECTORY_VARIABLE, target_directory(&root))
        .status();
    match status {
        Ok(finished) if finished.success() => ExitCode::SUCCESS,
        Ok(_finished) => ExitCode::FAILURE,
        Err(error) => refuse(&format!("cargo app: cargo could not be started: {error}")),
    }
}

/// Says something on standard error, where cargo's own progress goes.
fn say(line: &str) {
    let _written = writeln!(std::io::stderr(), "{line}");
}

/// Says something on standard error, and fails.
fn refuse(line: &str) -> ExitCode {
    say(line);
    ExitCode::FAILURE
}
