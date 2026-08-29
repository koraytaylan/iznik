//! `iznik uninstall <host>`: everything the bootstrap put on a host, removed.
//!
//! A tool that installs binaries on other people's machines owes them a way
//! to take them off, and a way that says what it took: the prefix and the
//! runtime directory, printed, so that what is gone can be seen to be gone.

use std::ffi::OsString;
use std::process::ExitCode;

use iznik_client::bootstrap::launch::{BOOTSTRAP_DEADLINE, BootstrapOptions};
use iznik_client::bootstrap::{Removed, uninstall};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, Transport};

use crate::output::{line, object, refusal, text};
use crate::{CLIENT_LAYER, TRANSPORT_LAYER, USAGE_EXIT_CODE, one_host, runtime};

/// What this subcommand takes.
const USAGE: &str = "usage: iznik uninstall <host>";

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(alias) = one_host(arguments) else {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, USAGE);
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    match removed(&alias) {
        Ok(gone) => {
            let _printed = line(
                &mut std::io::stdout(),
                &object(vec![
                    ("host", text(&alias)),
                    ("prefix", text(&gone.prefix.display().to_string())),
                    ("runtime", text(&gone.runtime.display().to_string())),
                ]),
            );
            ExitCode::SUCCESS
        }
        Err((layer, detail)) => {
            let _said = refusal(&mut std::io::stderr(), layer, &detail);
            ExitCode::FAILURE
        }
    }
}

/// Takes everything off the host, and says which layer refused when one did.
///
/// # Errors
///
/// The layer and what it said.
fn removed(alias: &str) -> Result<Removed, (&'static str, String)> {
    let held = runtime().map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let paths =
        ClientRuntimePaths::resolve().map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let transport = Transport::for_alias(alias, &paths, SshOptions::default());
    held.block_on(uninstall(
        &transport,
        &BootstrapOptions::default(),
        BOOTSTRAP_DEADLINE,
    ))
    .map_err(|source| (TRANSPORT_LAYER, source.to_string()))
}
