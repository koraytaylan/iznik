//! What a bootstrap decides, what it opens, and the words it fails with.
//!
//! The decision is made before anything is put on a host: a machine this build
//! carries no artifact for is told so from the probe's answer alone, rather
//! than discovered when a binary will not start. And because the daemon *is*
//! the sessions, a host running another version is connected to as it is; the
//! offer to replace it is carried back for somebody to accept.

use core::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_client, encode_to_server,
};
use iznik_protocol::model::{HostModel, decode_host_model};

use crate::bootstrap::probe::{
    Architecture, HostProbe, InstalledServer, OperatingSystem, PROBE_DEADLINE,
};
use crate::bootstrap::upload::{ArtifactSet, BINARY_NAME, UPLOAD_DEADLINE};
use crate::transport::Transport;
use crate::transport::channel::{ChannelError, ChannelOptions, RemoteChannel};

/// How long a connection to a host that needs nothing installed may take.
///
/// The first bootstrap of a host uploads a binary and is as slow as the link;
/// every one after it is this, and it is the one a person waits through.
pub const FAST_RECONNECT_BUDGET: Duration = Duration::from_secs(2);

/// How long the snapshot after a handshake may take.
pub const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(10);

/// How long a whole bootstrap that may have to install may take.
pub const BOOTSTRAP_DEADLINE: Duration = Duration::from_mins(6);

/// The directory the server goes in under a prefix.
const BINARY_DIRECTORY: &str = "bin";

/// What the bootstrap decided about a host, before it did any of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The host already has the version this build carries; nothing is sent.
    UpToDate,
    /// The host has no server this client may run.
    Install,
    /// The host has another version. The connection proceeds with the one
    /// that is there, and this is carried back so somebody can be asked.
    UpgradeAvailable {
        /// What is on the host.
        installed: InstalledServer,
        /// What this build would put there.
        bundled: InstalledServer,
    },
    /// This build has no artifact for the machine the host turned out to be.
    Unsupported {
        /// The triple that would have been needed.
        triple: String,
    },
}

impl Display for Decision {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Decision::UpToDate => formatter.write_str("up to date"),
            Decision::Install => formatter.write_str("install"),
            Decision::UpgradeAvailable { installed, bundled } => write!(
                formatter,
                "upgrade available from {} to {}",
                installed.crate_version, bundled.crate_version
            ),
            Decision::Unsupported { triple } => write!(formatter, "unsupported {triple}"),
        }
    }
}

/// Which part of a bootstrap did not work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Asking the host about itself.
    Probe,
    /// Putting the server there.
    Upload,
    /// Starting it and opening a link to it.
    Launch,
    /// Agreeing with it, and asking it what it holds.
    Handshake,
}

impl Display for Stage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Stage::Probe => "probing",
            Stage::Upload => "uploading",
            Stage::Launch => "launching",
            Stage::Handshake => "shaking hands",
        })
    }
}

/// Why a bootstrap did not finish, and how far it had got.
///
/// The stage is the point: a person told only that a host "failed" learns
/// nothing, and the four stages fail for four different reasons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapError {
    /// The host.
    pub host: String,
    /// What was being done.
    pub stage: Stage,
    /// What the remote said, or what went wrong here.
    pub detail: String,
}

impl Display for BootstrapError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let BootstrapError {
            host,
            stage,
            detail,
        } = self;
        write!(formatter, "{host}, {stage}: {detail}")
    }
}

impl core::error::Error for BootstrapError {}

/// Why an upgrade did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeError {
    /// The daemon is holding panes, and replacing it would end them.
    LivePanes {
        /// The host.
        host: String,
        /// How many panes it holds.
        count: usize,
    },
    /// Something the bootstrap itself could not do.
    Bootstrap(BootstrapError),
}

impl Display for UpgradeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            UpgradeError::LivePanes { host, count } => write!(
                formatter,
                "{host} is holding {count} pane(s) and replacing its server would end them; \
                 ask again with force when that is what you mean"
            ),
            UpgradeError::Bootstrap(source) => write!(formatter, "{source}"),
        }
    }
}

impl core::error::Error for UpgradeError {}

impl From<BootstrapError> for UpgradeError {
    fn from(source: BootstrapError) -> UpgradeError {
        UpgradeError::Bootstrap(source)
    }
}

/// Every timing a bootstrap runs under, so a case can shorten any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapOptions {
    /// How long the probe may take.
    pub probe_deadline: Duration,
    /// How long an upload may take.
    pub upload_deadline: Duration,
    /// How long the snapshot after the handshake may take.
    pub snapshot_deadline: Duration,
    /// How long one remote command of its own — stopping a daemon before it is
    /// replaced, taking an install off — may take.
    pub command_deadline: Duration,
    /// What the channel runs under.
    pub channel: ChannelOptions,
}

impl Default for BootstrapOptions {
    fn default() -> BootstrapOptions {
        BootstrapOptions {
            probe_deadline: PROBE_DEADLINE,
            upload_deadline: UPLOAD_DEADLINE,
            snapshot_deadline: SNAPSHOT_DEADLINE,
            command_deadline: PROBE_DEADLINE,
            channel: ChannelOptions::default(),
        }
    }
}

/// A host that is connected, and what it took to get there.
#[derive(Debug)]
pub struct Bootstrapped {
    /// What was decided before anything was done.
    pub decision: Decision,
    /// Where the server is on the host.
    pub server: PathBuf,
    /// Where the terminfo iznik carries is on the host, when it is anywhere.
    ///
    /// A pane on a host that has it is told `xterm-ghostty`; one on a host
    /// that does not is told `xterm-256color`, which is the nearest lie.
    pub terminfo: Option<PathBuf>,
    /// Why there is none, when there is none and a reason is known.
    pub terminfo_refused: Option<String>,
    /// What the host said it holds.
    pub snapshot: HostModel,
    /// The channel it is reached on.
    pub channel: RemoteChannel,
}

/// What this build carries, which is what an upgrade would install.
///
/// Every crate in this workspace takes its version from the workspace, so the
/// client's own version is the server's and no manifest is read to find it.
#[must_use]
pub fn bundled() -> InstalledServer {
    InstalledServer {
        crate_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_version: PROTOCOL_VERSION,
    }
}

/// The triple a probe's answer names.
#[must_use]
pub fn triple_of(found: &HostProbe) -> String {
    let machine = match found.architecture {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
    };
    match found.operating_system {
        OperatingSystem::Linux => format!("{machine}-unknown-linux-musl"),
        OperatingSystem::Darwin => format!("{machine}-apple-darwin"),
    }
}

/// Where the server is, or will be, under a probed prefix.
#[must_use]
pub fn server_path(found: &HostProbe) -> PathBuf {
    found.prefix.join(BINARY_DIRECTORY).join(BINARY_NAME)
}

/// What to do about a host, given what it said and what this build carries.
///
/// Nothing here touches the host, which is what makes "an unsupported host is
/// told so before anything is uploaded" a property rather than a promise.
#[must_use]
pub fn decide(found: &HostProbe, artifacts: &ArtifactSet, carried: &InstalledServer) -> Decision {
    let triple = triple_of(found);
    if artifacts.for_triple(&triple).is_err() {
        return Decision::Unsupported { triple };
    }
    let Some(installed) = found.server.clone() else {
        return Decision::Install;
    };
    if installed.crate_version == carried.crate_version
        && installed.protocol_version == carried.protocol_version
    {
        return Decision::UpToDate;
    }
    Decision::UpgradeAvailable {
        installed,
        bundled: carried.clone(),
    }
}

/// How many panes a host holds, across every session and every tab.
#[must_use]
pub fn live_panes(snapshot: &HostModel) -> usize {
    snapshot
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .map(|tab| tab.panes.len())
        .sum()
}

/// The moment `deadline` from now, or now if the clock cannot say.
#[must_use]
pub fn expiry(deadline: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(deadline).unwrap_or(now)
}

/// What is left of a whole bootstrap's budget, never more than one stage's own.
///
/// A stage that would run past the caller's deadline is given what remains and
/// fails as itself, which is how the error names the stage it happened in.
#[must_use]
pub fn left(expires: Instant, stage: Duration) -> Duration {
    stage.min(expires.saturating_duration_since(Instant::now()))
}

/// A refusal from a stage, in that stage's own words.
pub(crate) fn refused(host: &str, stage: Stage, detail: &impl Display) -> BootstrapError {
    BootstrapError {
        host: host.to_owned(),
        stage,
        detail: detail.to_string(),
    }
}

/// The stage a channel's failure belongs to.
///
/// Opening is two things at once — starting the server and agreeing with it —
/// and which of them failed is exactly what the reader needs: a link that
/// never opened is a path or an SSH problem, and a link that opened and then
/// disagreed is a version problem.
fn stage_of(source: &ChannelError) -> Stage {
    match source {
        // A link that opened and then disagreed, said the wrong thing, or said
        // nothing at all is the handshake. Time was given and the greeting did
        // not come; a server that could not be started closes the link instead
        // and says why on its standard error.
        ChannelError::ProtocolVersion { .. }
        | ChannelError::Unexpected { .. }
        | ChannelError::Deadline { .. } => Stage::Handshake,
        _other => Stage::Launch,
    }
}

/// Opens a channel to the server on a host and asks it what it holds.
///
/// # Errors
///
/// A [`BootstrapError`] at [`Stage::Launch`] when the link cannot be opened or
/// the caller's whole budget is already spent, and at [`Stage::Handshake`]
/// when the server disagrees about the protocol, says something else, or says
/// nothing inside the time it was given.
pub async fn launch(
    transport: &Transport,
    server: Option<&Path>,
    options: &BootstrapOptions,
    expires: Instant,
) -> Result<(RemoteChannel, HostModel), BootstrapError> {
    let host = transport.alias();
    let allowed = left(expires, options.channel.open_deadline);
    if allowed.is_zero() {
        // Nothing was attempted, so nothing said nothing: a refusal that
        // called this a handshake would send a person to look at a server that
        // was never started.
        return Err(refused(
            &host,
            Stage::Launch,
            &"the whole budget was spent before the server could be started",
        ));
    }
    let opening = ChannelOptions {
        open_deadline: allowed,
        ..options.channel.clone()
    };
    let mut channel = RemoteChannel::open(transport, server, opening)
        .await
        .map_err(|source| refused(&host, stage_of(&source), &source))?;
    let held = snapshot(
        &mut channel,
        left(expires, options.snapshot_deadline),
        &host,
    )
    .await?;
    Ok((channel, held))
}

/// Asks a connected server for its model and waits for it.
///
/// Anything else that arrives first is passed over: a snapshot is what was
/// asked for, and a channel with no subscriptions has nothing else to say.
///
/// # Errors
///
/// A [`BootstrapError`] at [`Stage::Handshake`] when it does not come, or does
/// not decode.
pub async fn snapshot(
    channel: &mut RemoteChannel,
    deadline: Duration,
    host: &str,
) -> Result<HostModel, BootstrapError> {
    let asked = encode_to_server(&ToServer::SnapshotRequest)
        .map_err(|source| refused(host, Stage::Handshake, &source))?;
    channel
        .send(CHANNEL_CONTROL, &asked)
        .await
        .map_err(|source| refused(host, Stage::Handshake, &source))?;
    let expires = expiry(deadline);
    loop {
        let arrived = channel
            .next(expires)
            .await
            .map_err(|source| refused(host, Stage::Handshake, &source))?;
        if arrived.channel != CHANNEL_CONTROL {
            continue;
        }
        let message = decode_to_client(&arrived.payload)
            .map_err(|source| refused(host, Stage::Handshake, &source))?;
        match message {
            ToClient::Snapshot { payload, .. } => {
                return decode_host_model(&payload)
                    .map_err(|source| refused(host, Stage::Handshake, &source));
            }
            ToClient::Error { code, message } => {
                return Err(refused(
                    host,
                    Stage::Handshake,
                    &format!("the server refused the snapshot: {code:?}, {message}"),
                ));
            }
            // Anything else on the control channel is neither the answer nor a
            // refusal of it, and the loop keeps waiting for one of the two.
            _other => {}
        }
    }
}
