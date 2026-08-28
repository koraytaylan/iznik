//! The `upload` step: the artifact put on a real host, verified there, and
//! installed under a name nothing else could have written.
//!
//! What a scenario asks of this is what a person would ask of a bootstrap: is
//! the server where it said it would be, does it run, is the terminfo beside
//! it, and is there nothing left over from a link that dropped.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iznik_client::bootstrap::probe::{PROBE_DEADLINE, probe};
use iznik_client::bootstrap::upload::{ArtifactSet, UPLOAD_DEADLINE, upload};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, Transport};
use serde::Deserialize;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// Where the staged distribution tree is mounted in the containers.
const STAGED_DISTRIBUTION: &str = "/iznik/distribution";

/// What a scenario expects an upload to have left.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    /// Where the server should be.
    #[serde(default)]
    server: Option<String>,
    /// Whether a terminfo directory should have been made.
    #[serde(default)]
    terminfo: Option<bool>,
    /// What the upload should have refused with, when it is expected to
    /// refuse.
    #[serde(default)]
    refused: Option<String>,
}

/// The `[steps.upload]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// The host alias, as `~/.ssh/config` names it.
    alias: String,
    /// Where the artifacts are on the machine running this.
    #[serde(default)]
    artifacts: Option<PathBuf>,
    /// What the upload must have left.
    #[serde(default)]
    expect: Expect,
}

/// The triple a probe's answer names.
fn triple_of(found: &iznik_client::bootstrap::probe::HostProbe) -> String {
    use iznik_client::bootstrap::probe::{Architecture, OperatingSystem};
    let machine = match found.architecture {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
    };
    match found.operating_system {
        OperatingSystem::Linux => format!("{machine}-unknown-linux-musl"),
        OperatingSystem::Darwin => format!("{machine}-apple-darwin"),
    }
}

/// Runs the step and says what it established.
///
/// # Errors
///
/// The step's own words when the upload or what it left is not what was asked
/// for.
async fn drive(body: &Body) -> Result<String, String> {
    let paths = ClientRuntimePaths::resolve().map_err(|error| error.to_string())?;
    let transport = Transport::for_alias(&body.alias, &paths, SshOptions::default());
    let found = probe(&transport, PROBE_DEADLINE)
        .await
        .map_err(|error| error.to_string())?;
    let directory = body
        .artifacts
        .clone()
        .unwrap_or_else(|| PathBuf::from(STAGED_DISTRIBUTION));
    let artifacts = ArtifactSet::load(&directory).map_err(|error| error.to_string())?;
    let triple = triple_of(&found);
    let artifact = artifacts
        .for_triple(&triple)
        .map_err(|error| error.to_string())?;
    match upload(&transport, artifact, &found, UPLOAD_DEADLINE).await {
        Ok(installed) => {
            if let Some(words) = &body.expect.refused {
                return Err(format!(
                    "expected a refusal saying {words:?} and it installed {}",
                    installed.server.display()
                ));
            }
            if let Some(wanted) = &body.expect.server
                && installed.server.as_path() != std::path::Path::new(wanted)
            {
                return Err(format!(
                    "expected the server at {wanted} and it went to {}",
                    installed.server.display()
                ));
            }
            if let Some(wanted) = body.expect.terminfo
                && installed.terminfo.is_some() != wanted
            {
                return Err(format!(
                    "expected terminfo {wanted} and got {:?}",
                    installed.terminfo
                ));
            }
            Ok(format!(
                "installed {}{}",
                installed.server.display(),
                installed
                    .terminfo
                    .map(|held| format!(", terminfo {}", held.display()))
                    .unwrap_or_default()
            ))
        }
        Err(refusal) => {
            let said = refusal.to_string();
            let Some(words) = &body.expect.refused else {
                return Err(said);
            };
            if said.contains(words.as_str()) {
                Ok(format!("refused: {said}"))
            } else {
                Err(format!(
                    "expected a refusal saying {words:?} and got {said}"
                ))
            }
        }
    }
}

/// The `upload` step. Its body is the `[steps.upload]` table.
///
/// # Errors
///
/// [`StepError::Malformed`] when the body is not the table this expects, and
/// [`StepError::Input`] when a runtime cannot be built.
pub fn execute(
    _context: &Context,
    body: &toml::Value,
    timeout: Duration,
) -> Result<Outcome, StepError> {
    let asked: Body =
        body.clone()
            .try_into()
            .map_err(|error: toml::de::Error| StepError::Malformed {
                detail: format!("an `upload` step: {error}"),
            })?;
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;
    let started = Instant::now();
    let driven = runtime.block_on(async {
        tokio::time::timeout(timeout, drive(&asked))
            .await
            .unwrap_or_else(|_elapsed| Err(format!("the upload step ran past {timeout:?}")))
    });
    let duration = started.elapsed();
    Ok(match driven {
        Ok(summary) => Outcome {
            exit: Some(0),
            timed_out: false,
            duration,
            stdout: summary,
            stderr: String::new(),
        },
        Err(reason) => Outcome {
            exit: Some(1),
            timed_out: duration >= timeout,
            duration,
            stdout: String::new(),
            stderr: reason,
        },
    })
}
