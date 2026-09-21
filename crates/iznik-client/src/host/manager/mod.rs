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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::bootstrap::launch::{
    BOOTSTRAP_DEADLINE, BootstrapError, BootstrapOptions, UpgradeError,
};
use crate::bootstrap::uninstall;
use crate::bootstrap::upload::{ArtifactSet, UploadError};
use crate::commands::{PENDING_COMMAND_TIMEOUT, Submission, submit, withdraw};
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

/// How many orders a host's task may carry in a row before it reads its link
/// again.
///
/// Large enough that a paste or a burst of typing goes out as fast as it was
/// handed over, and bounded so that a caller who never stops ordering still
/// leaves the loop hearing what the host says.
pub const ORDERS_PER_TURN: usize = 64;

pub mod credit;
mod task;

/// How long a caller waits for a host's task to end before it is cut short.
///
/// A task part way through a bootstrap does not look at its orders until the
/// bootstrap finishes, and that may be minutes; the thread that asked for the
/// host to go is a person's, and it does not wait minutes.
pub const STOP_DEADLINE: Duration = Duration::from_secs(5);

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
        /// Its width in cells.
        columns: u16,
        /// Its height in cells.
        rows: u16,
        /// The bytes that reproduce it, which is what a caller redraws from.
        bytes: Vec<u8>,
    },
    /// The host's whole model, as it said it.
    ///
    /// Carried in the protocol's own encoding rather than as a value of this
    /// crate's, because what is above the manager is a C boundary and one
    /// schema is better than two. `model()` holds the same thing, decoded.
    Snapshot {
        /// The host.
        host: HostId,
        /// The generation it stands at.
        generation: Generation,
        /// The model, as `decode_host_model` reads it.
        payload: Vec<u8>,
    },
    /// One numbered change to it, in the same encoding.
    Delta {
        /// The host.
        host: HostId,
        /// The generation this change produces.
        generation: Generation,
        /// The change, as `decode_delta` reads it.
        payload: Vec<u8>,
    },
    /// A pane's output has stopped arriving on the channel it was on.
    Detached {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
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
        /// Delivery-bound credit for these bytes. Engine deliveries always carry it;
        /// synthetic/offline events may omit it and have only current-stream credit semantics.
        receipt: Option<credit::CreditReceipt>,
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
    /// The log an application asked for will not be written.
    ///
    /// Refused rather than shrugged at: somebody who names a file wants what
    /// went wrong written to it, and the one moment they would find out it
    /// was never written is the moment they go looking for the reason
    /// something failed.
    Log {
        /// The file that was asked for.
        path: PathBuf,
        /// Why it will not be.
        detail: String,
    },
    /// The pane is not carrying anything just now.
    ///
    /// Its own refusal rather than an unknown host, which is what a held and
    /// connected host would otherwise be called: between a reconnection and
    /// the host re-announcing its panes, a pane can be left holding a number
    /// that now belongs to another, and credit for it would go where no pane
    /// would ever receive it. Nothing is wrong, and there is nothing to do
    /// but wait for the screen that says where it went.
    NotCarrying {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
    },
    /// A lock the manager holds was left broken by a panic under it.
    ///
    /// Its own error rather than an empty answer: a manager that reported no
    /// hosts, or an unknown one, would have whoever is watching believe
    /// something about the world instead of about this program.
    Poisoned {
        /// Which lock.
        what: &'static str,
    },
    /// The connected server cannot decode the command, so it was not sent.
    ///
    /// A server built before a command existed refuses its frame as garbage
    /// and ends the connection on it, so a client sends a command only when
    /// the server advertised it can decode it. Upgrading the host is what
    /// makes the command — and the feature that wants it — available.
    Unsupported {
        /// The host.
        host: HostId,
        /// What the command is called, for the words a person reads.
        command: &'static str,
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
            ManagerError::Log { path, detail } => {
                write!(formatter, "the log at {}: {detail}", path.display())
            }
            ManagerError::NotCarrying { host, pane } => write!(
                formatter,
                "pane {} on {host} is not carrying anything just now",
                pane.0
            ),
            ManagerError::Poisoned { what } => {
                write!(formatter, "the manager's {what} was left broken by a panic")
            }
            ManagerError::UnknownHost { host } => write!(formatter, "{host} is not held"),
            ManagerError::Gone { host } => write!(formatter, "{host} is no longer running"),
            ManagerError::Unsupported { host, command } => write!(
                formatter,
                "{host} is running a server too old for {command}; upgrade the host to use it"
            ),
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
    /// Flow-control credit bound to the exact stream which earned it.
    Credit {
        /// Delivery or compatibility grant, checked again at the carrying boundary.
        receipt: credit::CreditReceipt,
    },
    /// A command already applied to the model, to be sent.
    Command {
        /// This client's number for it.
        id: CommandId,
        /// What was asked.
        command: SessionCommand,
    },
    /// Ask for a pane's screen as it stands.
    Screen {
        /// The pane.
        pane: PaneId,
    },
    /// Drop the channel and open another at once.
    Reconnect,
    /// Replace the server on this host with this build's, then reconnect.
    Upgrade {
        /// Whether to replace it even though it holds panes, which ends them.
        force: bool,
    },
    /// End the task.
    Stop,
}

/// Everything the tasks share.
#[derive(Debug)]
pub(crate) struct Shared {
    /// What the client knows.
    pub(crate) model: Mutex<ClientModel>,
    /// Current stream incarnations and once-only delivery credit.
    pub(crate) credit: Mutex<credit::CreditStreams>,
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

/// Where this process is already writing what this crate says, if anywhere.
///
/// A subscriber belongs to a process and not to a manager: there is one, it is
/// installed once, and it outlives whatever asked for it — so a client that is
/// freed does not give the file back. Remembering which file it was given is
/// what lets the second client be told the truth, that its own file will never
/// be written, rather than be handed a client and an empty file.
///
/// A lock rather than a cell written once, because looking, installing and
/// remembering have to be one act: two clients starting at once would
/// otherwise leave the one that lost the race told that something unnamed had
/// taken the log, which is true of nothing it could act on.
static WRITING: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Sends what this crate says to a file, when one was asked for.
///
/// Best effort about *who* is listening and exact about the file: a process
/// that already installed a subscriber of its own keeps it — the command-line
/// program does, and a manager inside it must not take that away — but a file
/// that was named and cannot be written is an error, because the person who
/// named it will go looking for what is in it.
///
/// # Errors
///
/// [`ManagerError::Log`] when the file cannot be opened for appending, when
/// this process is already writing its log somewhere else, and when something
/// outside this crate has already taken what it says — each of them a log
/// that will not be written, which is the one thing the caller must not be
/// left to discover by reading nothing.
fn write_to(path: Option<&std::path::Path>) -> Result<(), ManagerError> {
    let Some(path) = path else {
        return Ok(());
    };
    let mut writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(already) = writing.as_deref() {
        return taken(path, Some(already));
    }
    if let Some(holding) = path.parent() {
        std::fs::create_dir_all(holding).map_err(|source| ManagerError::Log {
            path: path.to_path_buf(),
            detail: source.to_string(),
        })?;
    }
    // Whether the file was already there decides what a refusal below may
    // clear up: a log somebody has been writing to is not this call's to
    // remove, and an empty one this call made is exactly what it must not
    // leave.
    let existed = path.exists();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| ManagerError::Log {
            path: path.to_path_buf(),
            detail: source.to_string(),
        })?;
    if tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .try_init()
        .is_err()
    {
        // Nothing of this crate's took it, because this holds the lock that
        // would have: it is the program iznik is embedded in, and the
        // subscriber it installed is the one that stays. The file this call
        // made for a log that will not be written goes with the refusal.
        if !existed {
            let _cleared = std::fs::remove_file(path);
        }
        return taken(path, None);
    }
    *writing = Some(path.to_path_buf());
    Ok(())
}

/// Whether a log already being written — to `already`, or by somebody this
/// crate cannot name — satisfies a request for `path`.
///
/// # Errors
///
/// [`ManagerError::Log`] when it is a different file, because that one will
/// never be written and the caller would find out by reading nothing.
fn taken(path: &std::path::Path, already: Option<&std::path::Path>) -> Result<(), ManagerError> {
    if already == Some(path) {
        return Ok(());
    }
    Err(ManagerError::Log {
        path: path.to_path_buf(),
        detail: already.map_or_else(
            || "something else in this program is already taking what iznik says".to_owned(),
            |named| {
                format!(
                    "this process is already writing its log to {}",
                    named.display()
                )
            },
        ),
    })
}

impl HostManager {
    /// A manager with nothing in it yet.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Runtime`] when a runtime cannot be built,
    /// [`ManagerError::Artifacts`] when this build's artifacts cannot be read,
    /// and [`ManagerError::Log`] when a log was asked for and cannot be
    /// written.
    pub fn new(options: ManagerOptions) -> Result<HostManager, ManagerError> {
        let runtime = RuntimeBuilder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|source| ManagerError::Runtime { source })?;
        let artifacts = ArtifactSet::load(&options.artifacts_directory)
            .map_err(|source| ManagerError::Artifacts { source })?;
        // Last of the three that can refuse, because it is the only one that
        // leaves a mark on the process: a log claimed by a manager that then
        // failed to be built would be claimed for the life of the program,
        // and the next attempt — with the configuration corrected — would be
        // refused for a client that never existed.
        //
        // So nothing about the two above it is ever in the file, and that is
        // the right way round: they are returned to whoever called, who is
        // still there to read them. What the log is for is everything after
        // this line, which happens on tasks of its own with nobody waiting on
        // a return value.
        write_to(options.log_path.as_deref())?;
        let shared = Arc::new(Shared {
            model: Mutex::new(ClientModel::default()),
            credit: Mutex::new(credit::CreditStreams::default()),
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
    ///
    /// A lock left broken by a panic answers with an empty model, because this
    /// has nothing to say it with; every method that can returns
    /// [`ManagerError::Poisoned`] instead.
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
        // Twice is once, while the first is still running. Starting a second
        // task for one alias would leave the first detached and still going:
        // two channels, two `ssh` children, and a model written by whichever
        // of them finished last. A task that has given up is not still
        // running, and asking for that host again is asking for it afresh.
        if self.hosts.lock().is_ok_and(|held| {
            held.get(&host)
                .is_some_and(|handle| !handle.orders.is_closed())
        }) {
            return;
        }
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
        if let Ok(mut credit) = self.shared.credit.lock() {
            credit.disconnect(&host);
        }
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

    /// Asks a host for a pane's screen as it stands.
    ///
    /// What comes back is a [`ManagerEvent::Screen`]: a client that has
    /// nothing drawn, or that has lost track of what it drew, redraws from it
    /// rather than from the beginning of the pane's life.
    ///
    /// # Errors
    ///
    /// As [`HostManager::reconnect`].
    pub fn screen(&self, alias: &str, pane: PaneId) -> Result<(), ManagerError> {
        self.order(alias, Order::Screen { pane })
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
        // Refused before it is shown or sent: a server that never advertised
        // this command refuses its frame as garbage and ends the connection on
        // it, and a command only the connected server's capabilities can
        // answer is not one to send. Nothing is recorded and nothing moves.
        let name = command.name();
        let supported = self
            .shared
            .with(&host, |view| command.is_supported_by(view.capabilities))
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })?;
        if !supported {
            return Err(ManagerError::Unsupported {
                host,
                command: name,
            });
        }
        let submission = self
            .shared
            .with(&host, |view| submit(view, asked, Instant::now()))
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })?;
        // The host was there a moment ago, under the lock; if it is not now,
        // the order below says so.
        if let Err(refused) = self.order(
            alias,
            Order::Command {
                id: submission.id,
                command,
            },
        ) {
            // It showed, and then it could not be sent. Leaving it up would
            // have the screen in a state nobody was ever asked about, and the
            // sweeper would give up on a command that never left.
            let _undone = self
                .shared
                .with(&host, |view| withdraw(view, submission.id));
            return Err(refused);
        }
        Ok(submission)
    }

    /// Replaces the server on a host with the one this build carries.
    ///
    /// The host stays held for the whole replacement. Its task is told to
    /// upgrade, stops its own channel, runs the bootstrap and reconnects — so
    /// every order that arrives meanwhile goes to the task that is still
    /// there and is held until the new link is up, rather than being refused
    /// because the alias went missing. That is what makes a still-drawing
    /// window's sizes and subscriptions survive an upgrade instead of racing
    /// it: taking the handle out and blocking on a bootstrap, which is what
    /// this used to do, made every one of them answer `workstation is not
    /// held` while the upgrade was still succeeding.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`] when no host of that name is held, and
    /// [`ManagerError::Gone`] when its task has ended.
    pub fn upgrade(&self, alias: &str, force: bool) -> Result<(), ManagerError> {
        self.order(alias, Order::Upgrade { force })
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
        match removed {
            Ok(_gone) => {}
            Err(source) => {
                // It is still on the host, so it is still held here: a host
                // nobody holds is a host nobody can ask again, and the only
                // way back would be to add it from nothing.
                self.add_host(alias);
                return Err(ManagerError::Uninstall { source });
            }
        }
        // Only once it really came off. A host still holding a server and no
        // longer held here is one nobody can take it off, and the only way
        // back is to add it again.
        if let Ok(mut model) = self.shared.model.lock() {
            let _dropped = model.remove(&host);
        }
        self.shared.publish(&ManagerEvent::Removed { host });
        Ok(())
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
        let mut held = self
            .hosts
            .lock()
            .map_err(|_broken| ManagerError::Poisoned { what: "hosts" })?;
        held.remove(host)
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })
    }

    /// Ends a host's task and waits for it to go, for a while.
    ///
    /// A task that is part way through a bootstrap will not look at its orders
    /// until the bootstrap is over, so the wait is bounded and what will not
    /// stop is cut short: the alternative is a person's thread held for as
    /// long as a slow link takes.
    fn end(&self, handle: HostHandle) {
        let _asked = handle.orders.send(Order::Stop);
        let task = handle.task;
        let cutting = task.abort_handle();
        let ended = self
            .runtime
            .block_on(async { tokio::time::timeout(STOP_DEADLINE, task).await });
        if ended.is_err() {
            cutting.abort();
        }
    }

    /// Sends one order to a host's task.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`] when no host of that name is held, and
    /// [`ManagerError::Gone`] when its task has ended.
    fn order(&self, alias: &str, order: Order) -> Result<(), ManagerError> {
        let host = HostId(alias.to_owned());
        let held = self
            .hosts
            .lock()
            .map_err(|_broken| ManagerError::Poisoned { what: "hosts" })?;
        let sent = held
            .get(&host)
            .map(|handle| handle.orders.send(order))
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
            self.end(handle);
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
