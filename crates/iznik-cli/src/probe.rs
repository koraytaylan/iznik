//! `iznik probe <host>`: the bootstrap's probe of a host, printed.
//!
//! What the bootstrap asks a host before it decides anything — what the
//! machine is, what is already installed on it, where iznik may put things —
//! answered as one object so that a script can read it and a person can see
//! why a bootstrap decided what it did.

use std::ffi::OsString;
use std::process::ExitCode;

use iznik_client::bootstrap::launch::BootstrapOptions;
use iznik_client::bootstrap::probe::{Architecture, HostProbe, OperatingSystem, probe};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, Transport};

use crate::output::{Value, line, object, refusal, text};
use crate::{CLIENT_LAYER, TRANSPORT_LAYER, USAGE_EXIT_CODE, one_host, runtime};

/// What this subcommand takes.
const USAGE: &str = "usage: iznik probe <host>";

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    let Some(alias) = one_host(arguments) else {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, USAGE);
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    match asked(&alias) {
        Ok(found) => {
            let _printed = line(&mut std::io::stdout(), &shaped(&alias, &found));
            ExitCode::SUCCESS
        }
        Err((layer, detail)) => {
            let _said = refusal(&mut std::io::stderr(), layer, &detail);
            ExitCode::FAILURE
        }
    }
}

/// Probes the host, and says which layer refused when one did.
///
/// # Errors
///
/// The layer and what it said.
fn asked(alias: &str) -> Result<HostProbe, (&'static str, String)> {
    let held = runtime().map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let paths =
        ClientRuntimePaths::resolve().map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let transport = Transport::for_alias(alias, &paths, SshOptions::default());
    let options = BootstrapOptions::default();
    held.block_on(probe(&transport, options.probe_deadline))
        .map_err(|source| (TRANSPORT_LAYER, source.to_string()))
}

/// What a machine runs, on the wire.
///
/// Spelled out rather than derived from how the enum prints itself: what
/// crosses to a script is a name this program chose, and renaming a variant
/// must be a change somebody makes here on purpose.
#[must_use]
pub fn running(held: OperatingSystem) -> &'static str {
    match held {
        OperatingSystem::Linux => "linux",
        OperatingSystem::Darwin => "darwin",
        OperatingSystem::Windows => "windows",
    }
}

/// And what it is.
#[must_use]
pub fn machine(held: Architecture) -> &'static str {
    match held {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
    }
}

/// The probe as one object.
fn shaped(alias: &str, found: &HostProbe) -> Value {
    object(vec![
        ("host", text(alias)),
        ("operating_system", text(running(found.operating_system))),
        ("architecture", text(machine(found.architecture))),
        (
            "server",
            found.server.as_ref().map_or(Value::Null, |installed| {
                object(vec![
                    ("version", text(&installed.crate_version)),
                    (
                        "protocol_version",
                        Value::Whole(u64::from(installed.protocol_version)),
                    ),
                ])
            }),
        ),
        ("terminfo_installed", Value::Truth(found.terminfo_installed)),
        ("tic_available", Value::Truth(found.tic_available)),
        ("prefix", text(&found.prefix.display().to_string())),
    ])
}
