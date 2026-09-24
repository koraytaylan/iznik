//! How a host's task reaches its host: bootstrapping it — or, for a socket on
//! this machine, simply opening it — and reading from its greeting what the
//! server is, what of what it says may be trusted, and whether this build has
//! a newer one to offer.

use std::sync::{Arc, Mutex};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::DaemonInstance;

use crate::bootstrap::bootstrap_watched;
use crate::bootstrap::launch::{BootstrapError, Decision, Stage, bundled, expiry, launch};
use crate::bootstrap::probe::InstalledServer;
use crate::host::identity::HostId;
use crate::host::manager::task::advance;
use crate::host::manager::{Shared, bootstrapping};
use crate::host::state::{HostEvent, HostStateMachine, UpgradeOffer, UpgradeReason};
use crate::transport::Transport;
use crate::transport::channel::{RemoteChannel, ServerHello};

/// What reaching a host got: its channel, what it holds, what its server says
/// it is, and a newer one if this build carries it.
pub(super) struct Reached {
    /// The channel.
    pub(super) channel: RemoteChannel,
    /// The model the host answered with.
    pub(super) snapshot: iznik_protocol::model::HostModel,
    /// What its server says it is.
    pub(super) version: String,
    /// What its server advertised it can decode.
    pub(super) capabilities: Capabilities,
    /// Which run of the daemon answered, when it said.
    pub(super) instance: Option<DaemonInstance>,
    /// A newer one, when this build carries one.
    pub(super) offer: Option<UpgradeOffer>,
    /// The run that answered, when it is another build of this version still
    /// running from before the binary under it was replaced.
    pub(super) superseded: Option<DaemonInstance>,
}

/// Bootstraps a host and opens a channel to it.
///
/// # Errors
///
/// The [`BootstrapError`] naming the stage that failed.
pub(super) async fn reach(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
) -> Result<Reached, BootstrapError> {
    let transport = Transport::for_alias(
        &host.0,
        &shared.options.runtime_paths,
        shared.options.ssh.clone(),
    );
    let options = bootstrapping(&shared.options);
    let deadline = shared.options.bootstrap_deadline;
    if host.local_socket().is_some() {
        // A socket on this machine: there is nothing to probe and nothing to
        // install, and `unix:` is the alias this crate owns. It is a daemon
        // too, and it may be one this build did not start: its greeting says
        // whether anything is missing, and an offer is the only honest thing
        // to make of that.
        let (channel, snapshot) = launch(&transport, None, &options, expiry(deadline)).await?;
        return Ok(reached(
            host,
            shared,
            channel,
            snapshot,
            &Decision::UpToDate,
        ));
    }
    let watching = |stage: Stage| {
        let _reported = advance(shared, host, machine, HostEvent::Reached { stage });
    };
    let connected =
        bootstrap_watched(&transport, &shared.artifacts, &options, deadline, &watching).await?;
    Ok(reached(
        host,
        shared,
        connected.channel,
        connected.snapshot,
        &connected.decision,
    ))
}

/// What a channel the host answered on says, read against what the bootstrap
/// decided about it.
fn reached(
    host: &HostId,
    shared: &Shared,
    channel: RemoteChannel,
    snapshot: iznik_protocol::model::HostModel,
    decision: &Decision,
) -> Reached {
    let greeting = channel.greeting();
    let instance = greeting.instance;
    let (stale, superseded) = superseded(host, shared, decision, instance);
    let offer = offer_for(greeting, decision, stale);
    let version = greeting.server_version.clone();
    let capabilities = trusted_capabilities(greeting, stale);
    Reached {
        channel,
        snapshot,
        version,
        capabilities,
        instance,
        offer,
        superseded,
    }
}

/// Whether the run of the daemon that answered is another build of this
/// version than this one, and the run to remember as such.
///
/// A bootstrap that found another build of this version on the host replaces
/// its binary, and a daemon that was already running does not notice: it goes
/// on answering, as the old build, under the version this build has. That run
/// is remembered, so a later connection that finds the binary right — and the
/// same run still answering — does not take it for this build either.
fn superseded(
    host: &HostId,
    shared: &Shared,
    decision: &Decision,
    instance: Option<DaemonInstance>,
) -> (bool, Option<DaemonInstance>) {
    if matches!(decision, Decision::Replace) {
        // A server that does not say which run it is cannot be told apart
        // from the next one, and is taken for the old build on this
        // connection only.
        return (true, instance);
    }
    let before = shared.with(host, |view| view.superseded).flatten();
    match (before, instance) {
        (Some(remembered), Some(now)) if remembered == now => (true, Some(remembered)),
        _otherwise => (false, None),
    }
}

/// The capabilities of a greeting this build may act on.
///
/// A capability bit means what the build that assigned it says it means, and
/// the only thing that identifies a build is its version — and, within one
/// version, its bytes. Two servers that both say "protocol 1" may have given
/// one bit number two different jobs — an unreleased local build did exactly
/// that — so a server that is not this build's own version, or is `stale`,
/// another build of it, is not interpreted at all: its advertisement is
/// dropped and every feature gated on a bit is unavailable to it. That is the
/// conservative answer, and it costs nothing real, because such a host is
/// offered an upgrade on that ground alone.
fn trusted_capabilities(greeting: &ServerHello, stale: bool) -> Capabilities {
    if greeting.server_version == bundled().crate_version && !stale {
        greeting.capabilities
    } else {
        Capabilities::from_bits(0)
    }
}

/// The upgrade offer a connection puts on the table, if any.
///
/// Five things offer one. A host the probe found another version on is offered
/// it for the version. A host reached over a local socket — where no probe ran
/// — is offered it when its greeting names another version. A host whose
/// daemon is `stale`, another build of this version, is offered it for the
/// build. A host of this build whose server is nevertheless missing
/// capabilities is offered it for the gap. The installed server is always read
/// from what the greeting said, never assumed to equal what the build carries.
fn offer_for(greeting: &ServerHello, decision: &Decision, stale: bool) -> Option<UpgradeOffer> {
    // What the greeting said the host runs, which is never assumed to be what
    // this build carries.
    let what_the_host_said = InstalledServer {
        crate_version: greeting.server_version.clone(),
        protocol_version: greeting.protocol_version,
    };
    // The probe's own answer is the one to offer when it found a version this
    // build does not carry.
    if let Decision::UpgradeAvailable {
        installed: found,
        bundled: carried,
    } = decision
    {
        return Some(UpgradeOffer {
            installed: found.clone(),
            bundled: carried.clone(),
            reason: UpgradeReason::Version,
        });
    }
    let carried = bundled();
    let reason = if what_the_host_said.crate_version != carried.crate_version {
        // A local socket, or a probe the greeting disagreed with: the version
        // alone is reason enough, and it is the honest one.
        UpgradeReason::Version
    } else if stale {
        UpgradeReason::Build
    } else if trusted_capabilities(greeting, stale)
        .missing_features()
        .bits()
        != 0
    {
        UpgradeReason::Capabilities
    } else {
        return None;
    };
    Some(UpgradeOffer {
        installed: what_the_host_said,
        bundled: carried,
        reason,
    })
}
