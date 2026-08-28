//! Several hosts at once, each on a task of its own.
//!
//! What this file is for is isolation. One host bootstrapping over a slow
//! link, one host that has stopped answering, one host being upgraded — none
//! of them may be visible in another, and no operation on a healthy host may
//! wait on a sick one. So every host gets a task, every task owns its own
//! channel and its own state machine, and the only thing they share is a model
//! behind a lock that is never held across anything that touches a network.
//!
//! And when a link comes back, every subscribed pane is resumed at the byte the
//! model holds rather than redrawn from a screen. That is the whole reason the
//! cursor is kept: a person who closed a laptop sees the output that arrived
//! while it was shut, not a fresh prompt.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{CommandId, PaneId, Sequence};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::bootstrap::launch::{
    BOOTSTRAP_DEADLINE, BootstrapError, BootstrapOptions, UpgradeError,
};
use crate::bootstrap::upload::{ArtifactSet, UploadError};
use crate::bootstrap::{uninstall, upgrade};
use crate::commands::{PENDING_COMMAND_TIMEOUT, Submission, submit};
use crate::host::identity::HostId;
use crate::host::manager::task::{give_up, serve};
use crate::host::state::{BackoffPolicy, HostState};
use crate::model::{ClientModel, HostView};
use crate::reduce::Notification;
use crate::transport::channel::ChannelOptions;
use crate::transport::ssh::SshOptions;
use crate::transport::{ClientRuntimePaths, Transport};

/// How often a host's task wakes when nothing is arriving.
///
/// It is what gives up on commands the host never answered, so it is also the
/// most a person waits past the timeout before being told.
pub const EXPIRE_INTERVAL: Duration = Duration::from_millis(500);

mod task;

/// The odd constant a host's jitter seed is mixed with, so that two hosts
/// added in one breath do not retry together.
const SEED_MIX: u64 = 0x9e37_79b9_7f4a_7c15;

/// Every timing and place the manager runs under.
#[derive(Clone, Debug)]
pub struct ManagerOptions {
    /// Where this build's artifacts are on this machine.
    pub artifacts_directory: PathBuf,
    /// Where the client keeps what belongs to this machine.
    pub runtime_paths: ClientRuntimePaths,
    /// What `ssh` runs under.
    pub ssh: SshOptions,
    /// What every channel runs under.
    pub channel: ChannelOptions,
    /// How long a host waits before trying again, and how far apart two hosts
    /// that failed together are kept.
    pub backoff: BackoffPolicy,
    /// How long a command may go unanswered.
    pub pending_command_timeout: Duration,
    /// Where to write a log, when one is wanted.
    pub log_path: Option<PathBuf>,
    /// The bootstrap's own timings.
    ///
    /// Its `channel` is not read: a channel has one set of timings and
    /// `channel` above is where they are said.
    pub bootstrap: BootstrapOptions,
    /// The whole budget one bootstrap may take.
    pub bootstrap_deadline: Duration,
    /// How often a host's task wakes when nothing is arriving.
    pub expire_interval: Duration,
}

impl ManagerOptions {
    /// The options a manager runs under when nothing is said but where the
    /// artifacts are and where this machine's own files go.
    #[must_use]
    pub fn new(artifacts_directory: PathBuf, runtime_paths: ClientRuntimePaths) -> ManagerOptions {
        ManagerOptions {
            artifacts_directory,
            runtime_paths,
            ssh: SshOptions::default(),
            channel: ChannelOptions::default(),
            backoff: BackoffPolicy::default(),
            pending_command_timeout: PENDING_COMMAND_TIMEOUT,
            log_path: None,
            bootstrap: BootstrapOptions::default(),
            bootstrap_deadline: BOOTSTRAP_DEADLINE,
            expire_interval: EXPIRE_INTERVAL,
        }
    }
}

/// Something the manager says happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagerEvent {
    /// A host moved from one state to another.
    Moved {
        /// The host.
        host: HostId,
        /// Where it is now.
        state: HostState,
    },
    /// A host sent a screen for a pane; whatever is drawn is replaced by it.
    Screen {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
        /// The byte the screen is exact at.
        sequence: Sequence,
    },
    /// A subscribed pane's output, as it arrives.
    ///
    /// The sequence is the byte the first of them is, so a caller that cares
    /// can see for itself that a stream carried on across a reconnection
    /// rather than starting again.
    Bytes {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
        /// The byte the first of these is.
        sequence: Sequence,
        /// What arrived.
        bytes: Vec<u8>,
    },
    /// Something worth telling whoever is watching.
    Notify(Notification),
    /// A host is no longer held at all.
    Removed {
        /// The host.
        host: HostId,
    },
}

/// Why the manager could not do something.
#[derive(Debug)]
pub enum ManagerError {
    /// Its runtime could not be built.
    Runtime {
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The artifacts could not be read from this machine.
    Artifacts {
        /// What went wrong.
        source: UploadError,
    },
    /// No host of that name is held.
    UnknownHost {
        /// The name that was asked for.
        host: HostId,
    },
    /// The host's task has ended and is not taking orders.
    Gone {
        /// The host.
        host: HostId,
    },
    /// The host refused an upgrade, or could not be reached for one.
    Upgrade {
        /// What went wrong.
        source: UpgradeError,
    },
    /// The host could not be taken off.
    Uninstall {
        /// What went wrong.
        source: BootstrapError,
    },
}

impl core::fmt::Display for ManagerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ManagerError::Runtime { source } => {
                write!(formatter, "the manager's runtime: {source}")
            }
            ManagerError::Artifacts { source } => write!(formatter, "{source}"),
            ManagerError::UnknownHost { host } => write!(formatter, "{host} is not held"),
            ManagerError::Gone { host } => write!(formatter, "{host} is no longer running"),
            ManagerError::Upgrade { source } => write!(formatter, "{source}"),
            ManagerError::Uninstall { source } => write!(formatter, "{source}"),
        }
    }
}

impl core::error::Error for ManagerError {}

/// What an operation asks a host's own task to do.
#[derive(Clone, Debug)]
pub(crate) enum Order {
    /// Begin delivery of a pane's output.
    Subscribe {
        /// The pane.
        pane: PaneId,
    },
    /// End it.
    Unsubscribe {
        /// The pane.
        pane: PaneId,
    },
    /// Keystrokes.
    Input {
        /// The pane.
        pane: PaneId,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// A new size.
    Resize {
        /// The pane.
        pane: PaneId,
        /// Its width in cells.
        columns: u16,
        /// Its height in cells.
        rows: u16,
    },
    /// Which pane the person is looking at.
    Focus {
        /// The pane, or none.
        pane: Option<PaneId>,
    },
    /// Flow-control credit for a pane's channel.
    Credit {
        /// The channel.
        channel: u8,
        /// The bytes consumed.
        bytes: u32,
    },
    /// A command already applied to the model, to be sent.
    Command {
        /// This client's number for it.
        id: CommandId,
        /// What was asked.
        command: SessionCommand,
    },
    /// Drop the channel and open another at once.
    Reconnect,
    /// End the task.
    Stop,
}

/// Everything the tasks share.
#[derive(Debug)]
pub(crate) struct Shared {
    /// What the client knows.
    pub(crate) model: Mutex<ClientModel>,
    /// Everyone listening for events.
    pub(crate) listeners: Mutex<Vec<Sender<ManagerEvent>>>,
    /// What everything runs under.
    pub(crate) options: ManagerOptions,
    /// What this build carries.
    pub(crate) artifacts: ArtifactSet,
}

impl Shared {
    /// Tells everyone listening, and forgets the ones that have gone.
    pub(crate) fn publish(&self, event: &ManagerEvent) {
        let Ok(mut listeners) = self.listeners.lock() else {
            return;
        };
        listeners.retain(|held| held.send(event.clone()).is_ok());
    }

    /// Does something to one host's view, if the manager still holds it.
    pub(crate) fn with<Answer>(
        &self,
        host: &HostId,
        doing: impl FnOnce(&mut HostView) -> Answer,
    ) -> Option<Answer> {
        let mut model = self.model.lock().ok()?;
        model.host_mut(host).map(doing)
    }
}

/// One host's task, and the way to reach it.
#[derive(Debug)]
struct HostHandle {
    /// What it takes orders on.
    orders: UnboundedSender<Order>,
    /// The task itself, so it can be waited for.
    task: tokio::task::JoinHandle<()>,
}

/// Several hosts at once, each on a task of its own.
///
/// It owns its runtime, so every method here is a plain call from whatever
/// thread has one — and none of them may be called from inside that runtime,
/// because the ones that wait, wait on it.
#[derive(Debug)]
pub struct HostManager {
    /// The runtime every host's task runs on.
    runtime: Runtime,
    /// What the tasks share.
    shared: Arc<Shared>,
    /// One handle per host.
    hosts: Mutex<BTreeMap<HostId, HostHandle>>,
    /// The task that gives up on commands nobody answered.
    sweeper: tokio::task::JoinHandle<()>,
}

impl HostManager {
    /// A manager with nothing in it yet.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Runtime`] when a runtime cannot be built, and
    /// [`ManagerError::Artifacts`] when this build's artifacts cannot be read.
    pub fn new(options: ManagerOptions) -> Result<HostManager, ManagerError> {
        let runtime = RuntimeBuilder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|source| ManagerError::Runtime { source })?;
        let artifacts = ArtifactSet::load(&options.artifacts_directory)
            .map_err(|source| ManagerError::Artifacts { source })?;
        let shared = Arc::new(Shared {
            model: Mutex::new(ClientModel::default()),
            listeners: Mutex::new(Vec::new()),
            options,
            artifacts,
        });
        // Giving up on a command is a decision about a clock and a model, and
        // nothing else: it belongs where it happens whether a host is
        // connected, reconnecting or waiting out a backoff. A host's own task
        // is in none of those places at the same time.
        let sweeping = Arc::clone(&shared);
        let sweeper = runtime.spawn(sweep(sweeping));
        Ok(HostManager {
            runtime,
            shared,
            hosts: Mutex::new(BTreeMap::new()),
            sweeper,
        })
    }

    /// A stream of everything the manager says. Every caller gets its own.
    #[must_use]
    pub fn events(&self) -> Receiver<ManagerEvent> {
        let (sender, receiver) = channel();
        if let Ok(mut listeners) = self.shared.listeners.lock() {
            listeners.push(sender);
        }
        receiver
    }

    /// What the client knows, as it stands.
    #[must_use]
    pub fn model(&self) -> ClientModel {
        self.shared
            .model
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    /// Begins holding a host, and connecting to it.
    ///
    /// Returns at once: the connecting is the host's own task's business, and
    /// a manager that waited here would be a manager that waits on the slowest
    /// host somebody named.
    pub fn add_host(&self, alias: &str) {
        let host = HostId(alias.to_owned());
        if let Ok(mut model) = self.shared.model.lock()
            && model.host(&host).is_none()
        {
            let _first = model.insert(host.clone(), HostView::default());
        }
        let (orders, taking) = unbounded_channel();
        let shared = Arc::clone(&self.shared);
        let named = host.clone();
        let task = self.runtime.spawn(serve(named, shared, taking));
        if let Ok(mut hosts) = self.hosts.lock() {
            let _replaced = hosts.insert(host, HostHandle { orders, task });
        }
    }

    /// Stops holding a host: its task ends, its channel closes, and what the
    /// client knew about it is forgotten.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`] when no host of that name is held.
    pub fn remove_host(&self, alias: &str) -> Result<(), ManagerError> {
        let host = HostId(alias.to_owned());
        let handle = self.take(&host)?;
        self.end(handle);
        if let Ok(mut model) = self.shared.model.lock() {
            let _gone = model.remove(&host);
        }
        self.shared.publish(&ManagerEvent::Removed { host });
        Ok(())
    }

    /// Drops a host's channel and opens another at once, without waiting out
    /// its backoff.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`] or [`ManagerError::Gone`].
    pub fn reconnect(&self, alias: &str) -> Result<(), ManagerError> {
        self.order(alias, Order::Reconnect)
    }

    /// Begins delivery of a pane's output.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn subscribe(&self, alias: &str, pane: PaneId) -> Result<(), ManagerError> {
        self.order(alias, Order::Subscribe { pane })
    }

    /// Ends it.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn unsubscribe(&self, alias: &str, pane: PaneId) -> Result<(), ManagerError> {
        self.order(alias, Order::Unsubscribe { pane })
    }

    /// Sends keystrokes to a pane.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn input(&self, alias: &str, pane: PaneId, bytes: Vec<u8>) -> Result<(), ManagerError> {
        self.order(alias, Order::Input { pane, bytes })
    }

    /// Tells a pane it is another size.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn resize(
        &self,
        alias: &str,
        pane: PaneId,
        columns: u16,
        rows: u16,
    ) -> Result<(), ManagerError> {
        self.order(
            alias,
            Order::Resize {
                pane,
                columns,
                rows,
            },
        )
    }

    /// Says which pane the person is looking at.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn focus(&self, alias: &str, pane: Option<PaneId>) -> Result<(), ManagerError> {
        if let Ok(mut model) = self.shared.model.lock()
            && let Some(view) = model.host_mut(&HostId(alias.to_owned()))
        {
            view.focus = pane;
        }
        self.order(alias, Order::Focus { pane })
    }

    /// Returns flow-control credit for a pane's channel.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn credit(&self, alias: &str, channel: u8, bytes: u32) -> Result<(), ManagerError> {
        if let Ok(mut model) = self.shared.model.lock()
            && let Some(view) = model.host_mut(&HostId(alias.to_owned()))
            && let Some(pane) = view.carrying(channel)
            && let Some(held) = view.subscription_mut(pane)
        {
            held.grant(u64::from(bytes));
        }
        self.order(alias, Order::Credit { channel, bytes })
    }

    /// Sends a session command, showing what it does if what it does is beyond
    /// doubt.
    ///
    /// The local effect is applied here, under the lock, and the command goes
    /// to the host's task to be written — so nothing waits on a network with
    /// the model held.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`] or [`ManagerError::Gone`].
    pub fn command(
        &self,
        alias: &str,
        command: SessionCommand,
    ) -> Result<Submission, ManagerError> {
        let host = HostId(alias.to_owned());
        let asked = command.clone();
        let submission = self
            .shared
            .with(&host, |view| submit(view, asked, Instant::now()))
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })?;
        self.order(
            alias,
            Order::Command {
                id: submission.id,
                command,
            },
        )?;
        Ok(submission)
    }

    /// Replaces the server on a host with the one this build carries.
    ///
    /// The host's task is stopped first, because an upgrade ends the daemon
    /// its channel is talking to; it is started again afterwards, and
    /// reconnects.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`], or [`ManagerError::Upgrade`] carrying
    /// the refusal — the live panes it would have ended, or the stage that
    /// failed.
    pub fn upgrade(&self, alias: &str, force: bool) -> Result<(), ManagerError> {
        let host = HostId(alias.to_owned());
        let handle = self.take(&host)?;
        self.end(handle);
        let transport = self.reach(&host);
        let options = self.shared.options.clone();
        let artifacts = &self.shared.artifacts;
        let replaced = self.runtime.block_on(upgrade(
            &transport,
            artifacts,
            &bootstrapping(&options),
            force,
            options.bootstrap_deadline,
        ));
        self.add_host(alias);
        replaced.map_err(|source| ManagerError::Upgrade { source })
    }

    /// Takes iznik off a host, and stops holding it.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`], or [`ManagerError::Uninstall`] carrying
    /// what went wrong.
    pub fn uninstall(&self, alias: &str) -> Result<(), ManagerError> {
        let host = HostId(alias.to_owned());
        let handle = self.take(&host)?;
        self.end(handle);
        let transport = self.reach(&host);
        let options = self.shared.options.clone();
        let removed = self.runtime.block_on(uninstall(
            &transport,
            &bootstrapping(&options),
            options.bootstrap_deadline,
        ));
        if let Ok(mut model) = self.shared.model.lock() {
            let _gone = model.remove(&host);
        }
        self.shared.publish(&ManagerEvent::Removed { host });
        removed
            .map(|_gone| ())
            .map_err(|source| ManagerError::Uninstall { source })
    }

    /// The transport that reaches a host.
    fn reach(&self, host: &HostId) -> Transport {
        Transport::for_alias(
            &host.0,
            &self.shared.options.runtime_paths,
            self.shared.options.ssh.clone(),
        )
    }

    /// Takes a host's handle out, or says it is not held.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`].
    fn take(&self, host: &HostId) -> Result<HostHandle, ManagerError> {
        self.hosts
            .lock()
            .ok()
            .and_then(|mut held| held.remove(host))
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })
    }

    /// Ends a host's task and waits for it to go.
    fn end(&self, handle: HostHandle) {
        let _asked = handle.orders.send(Order::Stop);
        let _ended = self.runtime.block_on(handle.task);
    }

    /// Sends one order to a host's task.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`] when no host of that name is held, and
    /// [`ManagerError::Gone`] when its task has ended.
    fn order(&self, alias: &str, order: Order) -> Result<(), ManagerError> {
        let host = HostId(alias.to_owned());
        let sent = self
            .hosts
            .lock()
            .ok()
            .and_then(|held| held.get(&host).map(|handle| handle.orders.send(order)))
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })?;
        sent.map_err(|_gone| ManagerError::Gone { host })
    }
}

impl Drop for HostManager {
    fn drop(&mut self) {
        let taken: Vec<HostHandle> = self
            .hosts
            .lock()
            .map(|mut held| std::mem::take(&mut *held).into_values().collect())
            .unwrap_or_default();
        for handle in taken {
            let _asked = handle.orders.send(Order::Stop);
        }
        self.sweeper.abort();
    }
}

/// Gives up on every host's unanswered commands, for as long as the manager
/// lives.
async fn sweep(shared: Arc<Shared>) {
    loop {
        tokio::time::sleep(shared.options.expire_interval).await;
        let held: Vec<HostId> = shared
            .model
            .lock()
            .map(|model| model.hosts.keys().cloned().collect())
            .unwrap_or_default();
        for host in held {
            give_up(&host, &shared);
        }
    }
}

/// The bootstrap's options, with the channel's own timings put where the
/// bootstrap reads them.
pub(crate) fn bootstrapping(options: &ManagerOptions) -> BootstrapOptions {
    BootstrapOptions {
        channel: options.channel.clone(),
        ..options.bootstrap.clone()
    }
}

/// A seed of this host's own, so that four hosts whose links died together do
/// not come back together.
fn seeded(policy: &BackoffPolicy, host: &HostId) -> BackoffPolicy {
    let mut mixed = policy.seed;
    for byte in host.0.bytes() {
        mixed = mixed.rotate_left(u32::from(byte)) ^ u64::from(byte);
        mixed = mixed.wrapping_mul(SEED_MIX);
    }
    BackoffPolicy {
        seed: mixed,
        ..*policy
    }
}
