//! `cargo app` (`xtask app`): build the servers the application installs on
//! ssh hosts, then run the application.
//!
//! A bundle carries its servers; a workspace build has what
//! `cargo xtask distribution` wrote into cargo's target directory, where the
//! application looks for them. This is that build and `cargo run` as one
//! command, with cargo's progress on the terminal for both, so a person never
//! has to know the servers are a separate step. Up to date, each server build
//! takes about a second; after a change to the server, it is rebuilt.
//!
//! Every Linux architecture is built by default, not only this machine's. A
//! host is whatever it is, and a server for the wrong architecture is one the
//! application will refuse to install — leaving that host running whatever
//! older build it already had, which is how a fix can be built and never reach
//! the machine it was for.

use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, ExitCode};

use iznik_harness::process::Output;

use crate::distribution::{build_with_output, linux, target_directory, workspace_root};

/// The flag naming a server to build, repeatable.
const TARGET_FLAG: &str = "--target";

/// The usage line, shared by the refusal and `--help`.
const USAGE_LINE: &str = "usage: cargo app [--target <triple>]...";

/// The variable that tells cargo which target directory to use.
const TARGET_DIRECTORY_VARIABLE: &str = "CARGO_TARGET_DIR";

/// The servers built when no `--target` is given: every Linux architecture
/// this distributes, in a fixed order.
///
/// Not this machine's own alone. A host is whatever it is — an `arm64` laptop
/// talking to an `x86_64` workstation is the ordinary case — and a server built
/// for the wrong architecture is one the application will refuse to install,
/// leaving the host on whatever older build it already had. Preparing every
/// Linux server is what makes "the app carries what your hosts need" true
/// without a person having to know their machines' architectures first, and
/// it is cheap: up to date, each is a lock and a fingerprint check.
#[must_use]
pub fn default_servers() -> Vec<String> {
    linux::TARGETS
        .iter()
        .map(|triple| (*triple).to_owned())
        .collect()
}

/// The servers asked for: every `--target`, or every Linux server when none
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
        wanted = default_servers();
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
