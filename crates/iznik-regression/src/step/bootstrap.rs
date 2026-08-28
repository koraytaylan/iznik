//! The `bootstrap` step: a real host reached the way a person reaches one.
//!
//! Probe, decide, upload if it is needed, launch, hand shake, snapshot — and
//! the two verbs that undo it. What a scenario asks of this is what somebody
//! would ask of a bootstrap: what did it decide, how long did it take, how
//! many panes would an upgrade have ended, and when it refused, where.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iznik_client::bootstrap::launch::{
    BOOTSTRAP_DEADLINE, BootstrapError, BootstrapOptions, UpgradeError, live_panes,
};
use iznik_client::bootstrap::upload::ArtifactSet;
use iznik_client::bootstrap::{bootstrap, uninstall, upgrade};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, Transport};
use serde::Deserialize;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// Where the staged distribution tree is mounted in the containers.
const STAGED_DISTRIBUTION: &str = "/iznik/distribution";

/// What the step does to the host.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Action {
    /// Connect, installing if the host has nothing this client may run.
    #[default]
    Bootstrap,
    /// Replace the server that is there with this build's.
    Upgrade,
    /// Take iznik off the host.
    Uninstall,
}

/// What a scenario expects of one bootstrap.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    /// Words the decision must contain, as the decision says itself.
    #[serde(default)]
    decision: Option<String>,
    /// The stage a refusal must name.
    #[serde(default)]
    stage: Option<String>,
    /// How many panes the host must be holding.
    #[serde(default)]
    live_panes: Option<usize>,
    /// Words a refusal must contain, when one is expected.
    #[serde(default)]
    refused: Option<String>,
}

/// The `[steps.bootstrap]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// The host alias, as `~/.ssh/config` names it.
    alias: String,
    /// What to do to it.
    #[serde(default)]
    action: Action,
    /// Whether an upgrade may end the panes the host is holding.
    #[serde(default)]
    force: bool,
    /// Where the artifacts are on the machine running this.
    #[serde(default)]
    artifacts: Option<PathBuf>,
    /// The whole budget, in milliseconds.
    ///
    /// Given, it is enforced rather than measured: the bootstrap is run under
    /// it and fails if it runs past, which is what makes "a second connection
    /// finishes inside the fast-reconnect budget" a claim the step can hold
    /// rather than a number a person reads afterwards.
    #[serde(default)]
    budget_milliseconds: Option<u64>,
    /// What it must have done.
    #[serde(default)]
    expect: Expect,
}

/// The artifacts this step installs from.
///
/// # Errors
///
/// The step's own words when they cannot be read.
fn artifacts(body: &Body) -> Result<ArtifactSet, String> {
    let directory = body
        .artifacts
        .clone()
        .unwrap_or_else(|| PathBuf::from(STAGED_DISTRIBUTION));
    ArtifactSet::load(&directory).map_err(|error| error.to_string())
}

/// A refusal held against what the scenario expected of one.
///
/// # Errors
///
/// The step's own words when a refusal was not expected, or not this one.
fn refusal(error: &BootstrapError, expect: &Expect) -> Result<String, String> {
    let said = error.to_string();
    if expect.stage.is_none() && expect.refused.is_none() {
        return Err(said);
    }
    let stage = error.stage.to_string();
    if let Some(wanted) = &expect.stage
        && &stage != wanted
    {
        return Err(format!(
            "expected the {wanted} stage and it failed {stage}: {said}"
        ));
    }
    if let Some(words) = &expect.refused
        && !said.contains(words.as_str())
    {
        return Err(format!(
            "expected a refusal saying {words:?} and got {said}"
        ));
    }
    Ok(format!("refused at {stage}: {said}"))
}

/// Connects to the host, installing first if it needs it.
///
/// # Errors
///
/// The step's own words when the bootstrap or what it decided is not what was
/// asked for.
async fn connect(
    transport: &Transport,
    body: &Body,
    options: &BootstrapOptions,
    deadline: Duration,
) -> Result<String, String> {
    let held = artifacts(body)?;
    let started = Instant::now();
    let connected = match bootstrap(transport, &held, options, deadline).await {
        Ok(connected) => connected,
        Err(error) => return refusal(&error, &body.expect),
    };
    let taken = started.elapsed();
    if let Some(words) = &body.expect.refused {
        return Err(format!(
            "expected a refusal saying {words:?} and it connected"
        ));
    }
    let decision = connected.decision.to_string();
    let panes = live_panes(&connected.snapshot);
    connected.channel.close();
    if let Some(wanted) = &body.expect.decision
        && !decision.contains(wanted.as_str())
    {
        return Err(format!(
            "expected the decision {wanted:?} and it was {decision:?}"
        ));
    }
    if let Some(wanted) = body.expect.live_panes
        && panes != wanted
    {
        return Err(format!(
            "expected {wanted} pane(s) and the host holds {panes}"
        ));
    }
    Ok(format!(
        "{decision}, {panes} pane(s), server {}, in {}ms",
        connected.server.display(),
        taken.as_millis()
    ))
}

/// Replaces the server on the host, or says why it will not.
///
/// # Errors
///
/// The step's own words when the upgrade or its refusal is not what was asked
/// for.
async fn replace(
    transport: &Transport,
    body: &Body,
    options: &BootstrapOptions,
    deadline: Duration,
) -> Result<String, String> {
    let held = artifacts(body)?;
    match upgrade(transport, &held, options, body.force, deadline).await {
        Ok(()) => {
            if let Some(wanted) = body.expect.live_panes {
                return Err(format!(
                    "expected a refusal naming {wanted} pane(s) and it upgraded"
                ));
            }
            if let Some(words) = &body.expect.refused {
                return Err(format!(
                    "expected a refusal saying {words:?} and it upgraded"
                ));
            }
            Ok("upgraded".to_owned())
        }
        Err(UpgradeError::LivePanes { host, count }) => {
            if body.expect.live_panes == Some(count) {
                Ok(format!("refused: {host} holds {count} pane(s)"))
            } else {
                Err(format!(
                    "expected {:?} pane(s) and it refused naming {count}",
                    body.expect.live_panes
                ))
            }
        }
        Err(UpgradeError::Bootstrap(error)) => refusal(&error, &body.expect),
    }
}

/// Takes iznik off the host.
///
/// # Errors
///
/// The step's own words when it will not come off.
async fn remove(
    transport: &Transport,
    body: &Body,
    options: &BootstrapOptions,
    deadline: Duration,
) -> Result<String, String> {
    match uninstall(transport, options, deadline).await {
        Ok(gone) => Ok(format!(
            "removed {}, runtime {}",
            gone.prefix.display(),
            gone.runtime.display()
        )),
        Err(error) => refusal(&error, &body.expect),
    }
}

/// Runs the step and says what it established.
///
/// # Errors
///
/// The step's own words when what happened is not what was asked for.
async fn drive(body: &Body) -> Result<String, String> {
    let paths = ClientRuntimePaths::resolve().map_err(|error| error.to_string())?;
    let transport = Transport::for_alias(&body.alias, &paths, SshOptions::default());
    let options = BootstrapOptions::default();
    let deadline = body
        .budget_milliseconds
        .map_or(BOOTSTRAP_DEADLINE, Duration::from_millis);
    match body.action {
        Action::Bootstrap => connect(&transport, body, &options, deadline).await,
        Action::Upgrade => replace(&transport, body, &options, deadline).await,
        Action::Uninstall => remove(&transport, body, &options, deadline).await,
    }
}

/// The `bootstrap` step. Its body is the `[steps.bootstrap]` table.
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
                detail: format!("a `bootstrap` step: {error}"),
            })?;
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;
    let started = Instant::now();
    let driven = runtime.block_on(async {
        tokio::time::timeout(timeout, drive(&asked))
            .await
            .unwrap_or_else(|_elapsed| Err(format!("the bootstrap step ran past {timeout:?}")))
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
