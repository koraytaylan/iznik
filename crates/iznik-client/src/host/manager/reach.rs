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
        // install, and `unix:` is the alias this crate owns.
        let (channel, snapshot) = launch(&transport, None, &options, expiry(deadline)).await?;
        let greeting = channel.greeting();
        // A socket on this machine is a daemon too, and it may be one this
        // build did not start: its greeting says whether anything is missing,
        // and an offer is the only honest thing to make of that.
        let offer = offer_for(greeting, &Decision::UpToDate);
        let version = greeting.server_version.clone();
        let capabilities = trusted_capabilities(greeting);
        let instance = greeting.instance;
        return Ok(Reached {
            channel,
            snapshot,
            version,
            capabilities,
            instance,
            offer,
        });
    }
    let watching = |stage: Stage| {
        let _reported = advance(shared, host, machine, HostEvent::Reached { stage });
    };
    let connected =
        bootstrap_watched(&transport, &shared.artifacts, &options, deadline, &watching).await?;
    let greeting = connected.channel.greeting();
    let offer = offer_for(greeting, &connected.decision);
    let version = greeting.server_version.clone();
    let capabilities = trusted_capabilities(greeting);
    let instance = greeting.instance;
    Ok(Reached {
        channel: connected.channel,
        snapshot: connected.snapshot,
        version,
        capabilities,
        instance,
        offer,
    })
}

/// The capabilities of a greeting this build may act on.
///
/// A capability bit means what the build that assigned it says it means, and
/// the only thing that identifies a build is its version. Two servers that both
/// say "protocol 1" may have given one bit number two different jobs — an
/// unreleased local build did exactly that — so a server that is not this
/// build's own version is not interpreted at all: its advertisement is dropped
/// and every feature gated on a bit is unavailable to it. That is the
/// conservative answer, and it costs nothing real, because a host of another
/// version is offered an upgrade on that ground alone.
pub(super) fn trusted_capabilities(greeting: &ServerHello) -> Capabilities {
    if greeting.server_version == bundled().crate_version {
        greeting.capabilities
    } else {
        Capabilities::from_bits(0)
    }
}

/// The upgrade offer a connection puts on the table, if any.
///
/// Four things offer one. A host the probe found another version on is offered
/// it for the version. A host reached over a local socket — where no probe ran
/// — is offered it when its greeting names another version. A host of this
/// build's version whose server is nevertheless missing capabilities is offered
/// it for the gap. The installed server is always read from what the greeting
/// said, never assumed to equal what the build carries.
fn offer_for(greeting: &ServerHello, decision: &Decision) -> Option<UpgradeOffer> {
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
    if what_the_host_said.crate_version != carried.crate_version {
        // A local socket, or a probe the greeting disagreed with: the version
        // alone is reason enough, and it is the honest one.
        return Some(UpgradeOffer {
            installed: what_the_host_said,
            bundled: carried,
            reason: UpgradeReason::Version,
        });
    }
    if trusted_capabilities(greeting).missing_features().bits() != 0 {
        return Some(UpgradeOffer {
            installed: what_the_host_said,
            bundled: carried,
            reason: UpgradeReason::Capabilities,
        });
    }
    None
}
