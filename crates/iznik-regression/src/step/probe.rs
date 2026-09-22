//! The `probe` step: what a real host says about itself, and whether it is what
//! the scenario expected.
//!
//! The reading of a probe's answer is a pure function with a table of cases
//! beside it; what this adds is the answer itself, from a host arranged the
//! way the scenario wants it — fresh, or with a home its user cannot write.

use std::time::{Duration, Instant};

use iznik_client::bootstrap::probe::{
    Architecture, HostProbe, OperatingSystem, PROBE_DEADLINE, ProbeError, probe,
};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, Transport};
use serde::Deserialize;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// What a scenario expects a probe to have found. Every field is optional:
/// what a scenario does not name, it does not care about.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    /// What the host runs, as `uname -s` names it.
    #[serde(default)]
    system: Option<String>,
    /// What it is, as `uname -m` names it.
    #[serde(default)]
    machine: Option<String>,
    /// Whether a server is already installed there.
    #[serde(default)]
    server: Option<bool>,
    /// Whether the terminal's terminfo is installed.
    #[serde(default)]
    terminfo: Option<bool>,
    /// Whether `tic` is there.
    #[serde(default)]
    tic: Option<bool>,
    /// The prefix the probe settled on.
    #[serde(default)]
    prefix: Option<String>,
    /// What the probe refused with, when it is expected to refuse.
    #[serde(default)]
    refused: Option<String>,
}

/// The `[steps.probe]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// The host alias, as `~/.ssh/config` names it.
    alias: String,
    /// What the probe must have found.
    #[serde(default)]
    expect: Expect,
}

/// How the probe's own words name what it runs.
fn system_of(found: &HostProbe) -> &'static str {
    match found.operating_system {
        OperatingSystem::Linux => "Linux",
        OperatingSystem::Darwin => "Darwin",
        OperatingSystem::Windows => "Windows_NT",
    }
}

/// And what it is.
fn machine_of(found: &HostProbe) -> &'static str {
    match found.architecture {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
    }
}

/// Holds one field to what the scenario said about it.
///
/// # Errors
///
/// The step's own words when they differ.
fn agrees<Held: PartialEq + core::fmt::Debug>(
    named: &str,
    wanted: Option<Held>,
    found: &Held,
) -> Result<(), String> {
    match wanted {
        Some(wanted) if wanted != *found => {
            Err(format!("{named}: expected {wanted:?} and found {found:?}"))
        }
        _agreed => Ok(()),
    }
}

/// Holds a probe to everything the scenario said about it.
///
/// # Errors
///
/// The step's own words for the first field that differs.
fn matches(body: &Body, found: &HostProbe) -> Result<String, String> {
    agrees(
        "system",
        body.expect.system.clone(),
        &system_of(found).to_owned(),
    )?;
    agrees(
        "machine",
        body.expect.machine.clone(),
        &machine_of(found).to_owned(),
    )?;
    agrees("server", body.expect.server, &found.server.is_some())?;
    agrees("terminfo", body.expect.terminfo, &found.terminfo_installed)?;
    agrees("tic", body.expect.tic, &found.tic_available)?;
    agrees(
        "prefix",
        body.expect.prefix.clone(),
        &found.prefix.display().to_string(),
    )?;
    if let Some(words) = &body.expect.refused {
        return Err(format!(
            "expected a refusal saying {words:?} and the host was probed: {found:?}"
        ));
    }
    Ok(format!(
        "{} on {}, prefix {}",
        system_of(found),
        machine_of(found),
        found.prefix.display()
    ))
}

/// Runs the step and says what it established.
///
/// # Errors
///
/// The step's own words when the probe or its answer is not what was asked
/// for.
async fn drive(body: &Body) -> Result<String, String> {
    let paths = ClientRuntimePaths::resolve().map_err(|error| error.to_string())?;
    let transport = Transport::for_alias(&body.alias, &paths, SshOptions::default());
    match probe(&transport, PROBE_DEADLINE).await {
        Ok(found) => matches(body, &found),
        Err(refusal) => refused(body, &refusal),
    }
}

/// Holds a refusal to what the scenario expected of it.
///
/// # Errors
///
/// The refusal itself when none was expected, and the step's own words when
/// the one that came is not the one that was.
fn refused(body: &Body, refusal: &ProbeError) -> Result<String, String> {
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

/// The `probe` step. Its body is the `[steps.probe]` table.
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
                detail: format!("a `probe` step: {error}"),
            })?;
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;
    let started = Instant::now();
    let driven = runtime.block_on(async {
        tokio::time::timeout(timeout, drive(&asked))
            .await
            .unwrap_or_else(|_elapsed| Err(format!("the probe step ran past {timeout:?}")))
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
