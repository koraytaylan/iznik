//! An upgrade must never make a host "not held".
//!
//! Taking the host's handle out of the manager and blocking on the whole
//! bootstrap — which is what an upgrade used to do — leaves the alias absent
//! for seconds. A window that is still drawing keeps asking for pane sizes and
//! subscriptions, and every one of them is refused with `UnknownHost`, which is
//! the words "workstation is not held" a person saw while the upgrade was in
//! fact succeeding. The host is now held throughout: the ask is an order on the
//! host's own task, and the task runs the replacement while its order queue
//! stays open.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::{BackoffPolicy, HostState};
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_protocol::identity::PaneId;
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// The pane a session is made with.
const PANE: PaneId = PaneId(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A scratch directory of this case's own, removed when the guard drops.
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When it cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-upgrade-held-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the servers these cases stand up.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// The manager these cases drive, over an empty artifacts directory.
///
/// # Errors
///
/// When the runtime paths cannot be made or the manager cannot be built.
fn manager(held: &Scratch) -> Result<HostManager, Failed> {
    let artifacts = held.path.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let mut options = ManagerOptions::new(artifacts, paths);
    options.backoff = BackoffPolicy {
        initial: Duration::from_millis(20),
        maximum: Duration::from_millis(200),
        ..BackoffPolicy::default()
    };
    options.channel = ChannelOptions {
        ping_interval: Duration::from_millis(50),
        pong_deadline: Duration::from_millis(400),
        open_deadline: Duration::from_secs(5),
        greeting_deadline: Duration::from_millis(300),
    };
    options.expire_interval = Duration::from_millis(50);
    options.pending_command_timeout = Duration::from_millis(300);
    Ok(HostManager::new(options)?)
}

/// The alias that reaches a socket on this machine.
fn alias(socket: &std::path::Path) -> String {
    format!("{LOCAL_PREFIX}{}", socket.display())
}

/// Waits for a host to be connected.
///
/// # Errors
///
/// When it is not connected inside `PROMPT`.
fn await_connected(events: &Receiver<ManagerEvent>, host: &HostId) -> Result<(), Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if let ManagerEvent::Moved { host: said, state } = event
            && said == *host
            && matches!(state, HostState::Connected { .. })
        {
            return Ok(());
        }
    }
    Err(format!("{host:?} did not connect inside {PROMPT:?}").into())
}

/// An upgrade asked for on a live host keeps it held, and orders sent while it
/// runs are accepted rather than refused as an unknown host.
///
/// The window keeps asking for pane sizes and focus throughout an upgrade.
/// Taking the alias out of the manager and blocking on the bootstrap — what an
/// upgrade used to do — is what made those answer `UnknownHost`, the words
/// "workstation is not held" a person saw while the upgrade was in fact
/// succeeding. The host is now held for the whole replacement, and this is the
/// property that is asserted: the ask is accepted, the host is still in the
/// model, sizes and focus sent after it are accepted, and the state a window
/// reads says the server is being upgraded.
///
/// # Panics
///
/// When the upgrade is refused, the host leaves the model, or a size or focus
/// sent during the upgrade is refused.
#[test]
fn an_upgrade_keeps_the_host_held() {
    let case = || -> Result<(), Failed> {
        let held = scratch("live")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = HostId(alias(stack.socket()));
        manager.add_host(&host.0);
        await_connected(&events, &host)?;

        // The ask itself must be accepted.
        manager
            .upgrade(&host.0, true)
            .map_err(|error| format!("the upgrade was refused: {error}"))?;

        // And the host is still held: what the window asks for while an upgrade
        // runs is queued on the task, not refused because the alias went away.
        manager
            .resize(&host.0, PANE, 120, 40)
            .map_err(|error| format!("a size during the upgrade was refused: {error}"))?;
        manager
            .focus(&host.0, Some(PANE))
            .map_err(|error| format!("a focus during the upgrade was refused: {error}"))?;
        assert!(
            manager.model().host(&host).is_some(),
            "the host is still in the model while its server is replaced"
        );

        // And the window is told it is upgrading, so the wait reads as one.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        let mut upgrading = false;
        while Instant::now() < expires && !upgrading {
            let left = expires.saturating_duration_since(Instant::now());
            let Ok(event) = events.recv_timeout(left) else {
                break;
            };
            if let ManagerEvent::Moved { host: said, state } = event
                && said == host
                && matches!(state, HostState::Upgrading)
            {
                upgrading = true;
            }
        }
        assert!(upgrading, "the host is said to be upgrading its server");

        drop(manager);
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
