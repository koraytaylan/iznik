//! The `transport` step: the system `ssh` against the fixture's hosts, and
//! what each of its failures is called.
//!
//! Everything the transport does that a pure test cannot reach is here: that a
//! master is established and reused, that the second command over it is
//! faster than the first, that closing it ends it, and that a host nothing
//! answers on, a key the host refuses and a key that is not the one remembered
//! are three different messages rather than one.
//!
//! It runs in the engine container, which carries no agent and no keys of its
//! own beyond the one the fixture wrote, so what is proven is what a person's
//! machine would do.

use std::time::{Duration, Instant};

use iznik_client::transport::ssh::{SshError, SshOptions, classify};
use iznik_client::transport::{ClientRuntimePaths, Transport};
use serde::Deserialize;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// The command run when the step names none: the cheapest thing that proves a
/// connection was made.
const DEFAULT_COMMAND: &str = "true";

/// How much faster a reused master must be than the handshake that made it.
/// Half is a wide margin — a second handshake is many times the cost of a
/// command on an open one — and wide is what keeps this from measuring the
/// container host's mood.
const REUSE_MARGIN: u32 = 2;

/// What a step expects to happen.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Expect {
    /// The command runs and exits zero.
    Ok,
    /// Nothing answered.
    Unreachable,
    /// The host refused the credentials.
    AuthenticationFailed,
    /// The host's key is not the one remembered.
    HostKeyChanged,
    /// The connection worked and the command did not.
    RemoteCommandFailed,
}

/// The `[steps.transport]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// The host alias, as `~/.ssh/config` names it.
    alias: String,
    /// What to run there.
    #[serde(default)]
    command: Option<String>,
    /// How long a connection may take, shortened so an unanswered address is
    /// a fast failure rather than a wait.
    #[serde(default)]
    connect_timeout_seconds: Option<u64>,
    /// What must happen.
    expect: Expect,
    /// Whether to run the command twice and assert that a master was made,
    /// reused, and that the second was the faster.
    #[serde(default)]
    check_master: bool,
    /// Whether to end the master afterwards and assert that it is gone.
    #[serde(default)]
    close_master: bool,
}

/// What one run of the command came to.
struct Ran {
    /// Its exit status, if it had one.
    status: Option<i32>,
    /// What it said on standard error.
    stderr: String,
    /// How long it took.
    taken: Duration,
}

/// Runs the command once over the transport and says what came of it.
///
/// # Errors
///
/// When `ssh` cannot be started or its output cannot be read.
async fn run_once(transport: &Transport, command: &str) -> Result<Ran, String> {
    let Transport::Ssh(ssh) = transport else {
        return Err("a transport step needs an alias ssh reaches".to_owned());
    };
    let started = Instant::now();
    let spawned = ssh
        .spawn(&[command.to_owned()])
        .map_err(|error| error.to_string())?;
    let output = spawned
        .child
        .wait_with_output()
        .await
        .map_err(|source| format!("ssh could not be waited for: {source}"))?;
    Ok(Ran {
        status: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        taken: started.elapsed(),
    })
}

/// Whether what happened is what was expected, and what to say either way.
///
/// # Errors
///
/// The step's own words when it was something else.
fn matches(expected: Expect, ran: &Ran, alias: &str) -> Result<String, String> {
    if expected == Expect::Ok {
        return match ran.status {
            Some(0) => Ok(format!("{alias} answered in {:?}", ran.taken)),
            other => Err(format!(
                "{alias} was expected to answer and exited {other:?}: {}",
                ran.stderr.trim()
            )),
        };
    }
    let classified = classify(alias, ran.status, &ran.stderr);
    let named = match (&classified, expected) {
        (SshError::Unreachable { .. }, Expect::Unreachable)
        | (SshError::AuthenticationFailed { .. }, Expect::AuthenticationFailed)
        | (SshError::HostKeyChanged { .. }, Expect::HostKeyChanged)
        | (SshError::RemoteCommandFailed { .. }, Expect::RemoteCommandFailed) => true,
        _mismatch => false,
    };
    if named {
        Ok(format!("{alias}: {classified}"))
    } else {
        Err(format!(
            "{alias} was expected to be {expected:?} and was {classified}"
        ))
    }
}

/// Whether `ssh -O check` says a master is live for this alias.
///
/// # Errors
///
/// When the alias is not one `ssh` reaches, or `ssh` cannot be run.
async fn master_is_live(transport: &Transport) -> Result<bool, String> {
    let Transport::Ssh(ssh) = transport else {
        return Err("a transport step needs an alias ssh reaches".to_owned());
    };
    let mut arguments = ssh.arguments(&[]);
    let at = arguments.len().saturating_sub(1);
    arguments.splice(at..at, ["-O".to_owned(), "check".to_owned()]);
    let output = tokio::process::Command::new("ssh")
        .args(arguments)
        .output()
        .await
        .map_err(|source| format!("ssh -O check could not be run: {source}"))?;
    Ok(output.status.success())
}

/// Runs the step and says what it established.
///
/// # Errors
///
/// The step's own words when what happened is not what was expected.
async fn drive(body: &Body) -> Result<String, String> {
    let paths = ClientRuntimePaths::resolve().map_err(|error| error.to_string())?;
    let mut options = SshOptions::default();
    if let Some(seconds) = body.connect_timeout_seconds {
        options.connect_timeout = Duration::from_secs(seconds);
    }
    let transport = Transport::for_alias(&body.alias, &paths, options);
    let command = body.command.as_deref().unwrap_or(DEFAULT_COMMAND);
    let first = run_once(&transport, command).await?;
    let mut said = matches(body.expect, &first, &body.alias)?;
    if body.check_master {
        if !master_is_live(&transport).await? {
            return Err(format!("{}: no master was left behind", body.alias));
        }
        let again = run_once(&transport, command).await?;
        let _second = matches(body.expect, &again, &body.alias)?;
        let budget = first.taken.checked_div(REUSE_MARGIN).unwrap_or_default();
        if again.taken > budget {
            return Err(format!(
                "{}: the second command took {:?} against the first's {:?}, \
                 which is not a master being reused",
                body.alias, again.taken, first.taken
            ));
        }
        said = format!("{said}, then {:?} over the master", again.taken);
    }
    if body.close_master {
        let Transport::Ssh(ssh) = &transport else {
            return Err("a transport step needs an alias ssh reaches".to_owned());
        };
        let closing = ssh.close_master().map_err(|error| error.to_string())?;
        let _ended = closing
            .child
            .wait_with_output()
            .await
            .map_err(|source| format!("ssh -O exit could not be waited for: {source}"))?;
        if master_is_live(&transport).await? {
            return Err(format!("{}: the master outlived its exit", body.alias));
        }
        said = format!("{said}, and the master is gone");
    }
    Ok(said)
}

/// The `transport` step. Its body is the `[steps.transport]` table.
///
/// # Errors
///
/// [`StepError::Malformed`] when the body is not the table this expects, and
/// [`StepError::Input`] when a runtime cannot be built.
pub fn execute(
    _context: &Context,
    body: &toml::Value,
    _timeout: Duration,
) -> Result<Outcome, StepError> {
    let asked: Body =
        body.clone()
            .try_into()
            .map_err(|error: toml::de::Error| StepError::Malformed {
                detail: format!("a `transport` step: {error}"),
            })?;
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;
    let started = Instant::now();
    let driven = runtime.block_on(drive(&asked));
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
            timed_out: false,
            duration,
            stdout: String::new(),
            stderr: reason,
        },
    })
}
