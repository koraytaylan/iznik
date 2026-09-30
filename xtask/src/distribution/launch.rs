//! `cargo app` (`xtask app`): build the servers the application installs on
//! ssh hosts, then run the application.
//!
//! A bundle carries its servers; a workspace build has what
//! `cargo xtask distribution` wrote into cargo's target directory, where the
//! application looks for them. This is that build and a launch as one
//! command, with cargo's progress on the terminal for both, so a person never
//! has to know the servers are a separate step. Up to date, each server build
//! takes about a second; after a change to the server, it is rebuilt.
//!
//! On macOS the launch is a signed debug bundle. A process started with
//! `cargo run` has no bundle, and the Dock takes its icon from the bundle, so
//! this writes one and runs that. Linux and Windows keep `cargo run`; their
//! status item is installed by the process.
//!
//! Every Linux architecture is built by default, not only this machine's. A
//! host is whatever it is, and a server for the wrong architecture is one the
//! application will refuse to install — leaving that host running whatever
//! older build it already had, which is how a fix can be built and never reach
//! the machine it was for.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

use iznik_harness::process::Output;

use crate::distribution::{
    DISTRIBUTION_DIRECTORY, build_with_output, linux, signing, target_directory, workspace_root,
};

/// The flag naming a server to build, repeatable.
const TARGET_FLAG: &str = "--target";

/// The usage line, shared by the refusal and `--help`.
const USAGE_LINE: &str = "usage: cargo app [--target <triple>]...";

/// The variable that tells cargo which target directory to use.
const TARGET_DIRECTORY_VARIABLE: &str = "CARGO_TARGET_DIR";

/// The profile `cargo app` builds the application under.
const DEBUG_PROFILE: &str = "debug";

/// The package cargo builds.
const APPLICATION_PACKAGE: &str = "iznik-app";

/// The name the bundle writer gives the application.
const APPLICATION_NAME: &str = "iznik";

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
    // The application runs until a person closes it, so it has no deadline.
    // On macOS the Dock icon comes from the signed bundle this writes first.
    if cfg!(target_os = "macos") {
        start_bundled(&root)
    } else {
        start_direct(&root)
    }
}

/// `cargo run`. The status item is installed by the process.
fn start_direct(root: &Path) -> ExitCode {
    let status = cargo(root)
        .args(["run", "--locked", "--package", APPLICATION_PACKAGE])
        .status();
    command_exit(status, "cargo could not be started")
}

/// Build the application, write a debug bundle, sign it, and run that bundle.
fn start_bundled(root: &Path) -> ExitCode {
    let directory = target_directory(root);
    let built = cargo(root)
        .args(["build", "--locked", "--package", APPLICATION_PACKAGE])
        .status();
    if command_exit(built, "cargo could not be started") != ExitCode::SUCCESS {
        return ExitCode::FAILURE;
    }
    let binary = application_binary(&directory);
    let bundle = directory
        .join(DEBUG_PROFILE)
        .join(format!("{APPLICATION_NAME}.app"));
    let triple = format!("{}-apple-darwin", std::env::consts::ARCH);
    let servers = directory.join(DISTRIBUTION_DIRECTORY);
    let written = Command::new(&binary)
        .arg("--bundle")
        .arg(&triple)
        .arg(&binary)
        .arg(&bundle)
        .arg(env!("CARGO_PKG_VERSION"))
        .arg(&servers)
        .status();
    if command_exit(written, "the bundle writer could not be started") != ExitCode::SUCCESS {
        return ExitCode::FAILURE;
    }
    let signature = signing::Signature::from_environment();
    if let Err(error) = signing::sign_bundle(&bundle, &triple, &signature) {
        return refuse(&format!("cargo app: {error}"));
    }
    let executable = bundle.join("Contents").join("MacOS").join(APPLICATION_NAME);
    let status = Command::new(executable).current_dir(root).status();
    command_exit(status, "the application could not be started")
}

/// Cargo, pointed at this workspace and its target directory.
fn cargo(root: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(root)
        .env(TARGET_DIRECTORY_VARIABLE, target_directory(root));
    command
}

/// The application binary cargo just built.
fn application_binary(directory: &Path) -> PathBuf {
    let mut name = APPLICATION_PACKAGE.to_owned();
    if cfg!(target_os = "windows") {
        name.push_str(".exe");
    }
    directory.join(DEBUG_PROFILE).join(name)
}

/// The exit of a command. A failure to start is said here.
fn command_exit(status: std::io::Result<ExitStatus>, failure: &str) -> ExitCode {
    match status {
        Ok(completed) if completed.success() => ExitCode::SUCCESS,
        Ok(_completed) => ExitCode::FAILURE,
        Err(error) => refuse(&format!("cargo app: {failure}: {error}")),
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
