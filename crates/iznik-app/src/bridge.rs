//! The engine behind the window: the manager, one channel, and the two threads
//! that keep the window's thread out of everything that waits.
//!
//! `EngineBridge` is the whole of what this application knows about the client
//! engine. It builds the `HostManager` — which owns the private tokio runtime
//! every host's task runs on — and starts two threads of its own: one that
//! reads what the manager says and puts it on a channel, and one that performs
//! the operations whose calls wait for a host's task to stop. Nothing else in
//! this crate names the engine.
//!
//! # The one rule
//!
//! **The window's thread never calls the engine on a code path that waits on
//! the engine's own tasks.** `HostManager::remove_host`, `HostManager::upgrade`
//! and `HostManager::uninstall` each wait, inside the manager, for a host's
//! task to end: an upgrade stops the task first, because it ends the daemon
//! that task's channel is talking to, and taking a host off waits for the same
//! task to go. A task part way through a bootstrap does not look at its orders
//! until the bootstrap is over, which may be minutes. Called from the thread
//! that draws frames, that is a window which has stopped drawing for as long as
//! the slowest host somebody named takes — and a person cannot tell that from a
//! hang.
//!
//! Those three are therefore *orders*. The bridge hands one to the thread that
//! performs it and answers at once, and what it did arrives on the same channel
//! as everything else, as an [`EngineEvent::Finished`]. The calls the bridge
//! does make from the window's thread are the ones the manager documents as
//! returning at once: `HostManager::add_host` spawns a task, and the rest put
//! an order on one host's own channel.
//!
//! The one place the window's thread waits is [`EngineBridge`]'s `Drop`, which
//! ends the engine. It waits there for the reason the C boundary does (see
//! `iznik_client_free`): a program that is closing has no frame to draw, and
//! what it waits for is bounded — a host's task is stopped under the manager's
//! own deadline, and an upgrade or a taking-off under the bootstrap's.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::{self, JoinHandle};

use iznik_client::commands::Submission;
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{HostManager, ManagerError, ManagerEvent, ManagerOptions};
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::PaneId;

use crate::vt::{TerminalTheme, VtCommand, VtError, VtEvent, VtOutput, VtThread};

/// The name the thread that reads what the manager says is given, so that a
/// person reading a process's threads can tell what each of them is for.
const FORWARDING_THREAD_NAME: &str = "iznik-app-events";

/// And the name the thread that performs the operations is given.
const OPERATIONS_THREAD_NAME: &str = "iznik-app-orders";

/// One engine operation whose call waits on the engine's own tasks.
///
/// Named rather than anonymous so that its answer is legible: what comes back
/// on the channel says which operation it was about, and the words a person
/// reads are this type's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Stop holding a host, and forget what this client knew about it.
    Remove {
        /// The host.
        host: HostId,
    },
    /// Replace the server on a host with the one this build carries.
    Upgrade {
        /// The host.
        host: HostId,
        /// Whether to replace it even though it is holding panes, which ends
        /// them.
        force: bool,
    },
    /// Take iznik off a host, and stop holding it.
    Uninstall {
        /// The host.
        host: HostId,
    },
}

impl Operation {
    /// The host it is about.
    #[must_use]
    pub fn host(&self) -> &HostId {
        match self {
            Operation::Remove { host }
            | Operation::Upgrade { host, .. }
            | Operation::Uninstall { host } => host,
        }
    }
}

impl core::fmt::Display for Operation {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Operation::Remove { host } => write!(formatter, "removing {host}"),
            Operation::Upgrade { host, force: false } => write!(formatter, "upgrading {host}"),
            Operation::Upgrade { host, force: true } => {
                write!(formatter, "upgrading {host}, ending the panes it holds")
            }
            Operation::Uninstall { host } => write!(formatter, "taking iznik off {host}"),
        }
    }
}

/// Everything the engine says to the window.
#[derive(Debug)]
pub enum EngineEvent {
    /// One thing the manager said, forwarded exactly as it said it.
    ///
    /// Carried whole rather than read here: what an event means is the window's
    /// business, and one this application has no use for yet is still one it
    /// must be able to see.
    Said(ManagerEvent),
    /// An operation that had to be performed off the window's thread has
    /// finished, and this is what it was about and what it answered.
    Finished {
        /// What was asked.
        operation: Operation,
        /// What the manager answered: nothing, or why it would not.
        answer: Result<(), ManagerError>,
    },
}

/// Why the engine would not start, or would not take an order.
#[derive(Debug)]
pub enum EngineError {
    /// The manager would not be built.
    Manager(ManagerError),
    /// The person's ssh configuration could not be read or written.
    Configuration(crate::ssh_config::SshConfigError),
    /// A thread the engine runs on could not be started.
    Thread {
        /// What the operating system said.
        source: io::Error,
    },
    /// The engine has stopped, and is taking nothing more.
    Stopped,
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::Manager(source) => write!(formatter, "{source}"),
            EngineError::Configuration(source) => write!(formatter, "{source}"),
            EngineError::Thread { source } => {
                write!(formatter, "a thread the engine runs on: {source}")
            }
            EngineError::Stopped => formatter.write_str("the engine has stopped"),
        }
    }
}

impl core::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            EngineError::Manager(source) => Some(source),
            EngineError::Configuration(source) => Some(source),
            EngineError::Thread { source } => Some(source),
            EngineError::Stopped => None,
        }
    }
}

impl From<ManagerError> for EngineError {
    fn from(source: ManagerError) -> EngineError {
        EngineError::Manager(source)
    }
}

/// The engine, and the way to talk to it without ever waiting on it.
///
/// Owned by the window's thread and by nothing else: the events channel it
/// drains is a single-consumer one, and every other thread here is one it
/// started.
#[derive(Debug)]
pub struct EngineBridge {
    /// The manager, while the engine is running.
    ///
    /// An `Option` so that ending the engine can drop the last handle from
    /// inside `Drop`, which is what ends the manager's runtime and closes the
    /// stream the forwarding thread reads.
    manager: Option<Arc<HostManager>>,
    /// What the engine has said that the window has not read yet.
    events: Receiver<EngineEvent>,
    /// Where an operation that waits goes. `None` on it is not an operation:
    /// it is what tells the thread that there will be no more.
    orders: Sender<Option<Operation>>,
    /// The thread that reads what the manager says.
    forwarding: Option<JoinHandle<()>>,
    /// The thread that performs the operations.
    operations: Option<JoinHandle<()>>,
}

impl EngineBridge {
    /// The engine, with the manager's options built from the two places this
    /// machine decides: where this build's artifacts are, and where the files
    /// that belong to this machine go.
    ///
    /// # Errors
    ///
    /// [`EngineError::Manager`] when the manager will not be built — a runtime
    /// that cannot be started, artifacts that cannot be read, or a log that
    /// cannot be written — and [`EngineError::Thread`] when one of the two
    /// threads the engine reads and performs on cannot be started.
    pub fn start(
        artifacts_directory: PathBuf,
        runtime_paths: ClientRuntimePaths,
    ) -> Result<EngineBridge, EngineError> {
        EngineBridge::under(ManagerOptions::new(artifacts_directory, runtime_paths))
    }

    /// The same, with every place and every timing said in full.
    ///
    /// What a test uses: each of the options' backoffs, deadlines and intervals
    /// is a field with a named default, and a case about a host that fails
    /// takes milliseconds rather than the minute a person would have waited.
    ///
    /// # Errors
    ///
    /// As [`EngineBridge::start`].
    pub fn under(options: ManagerOptions) -> Result<EngineBridge, EngineError> {
        let manager = Arc::new(HostManager::new(options).map_err(EngineError::Manager)?);
        let (sender, events) = channel();
        let listening = manager.events();
        let forwarding = {
            let sending = sender.clone();
            thread::Builder::new()
                .name(FORWARDING_THREAD_NAME.to_owned())
                .spawn(move || relay(&listening, &sending))
                .map_err(|source| EngineError::Thread { source })?
        };
        let (orders, taking) = channel();
        let operations = {
            let holding = Arc::clone(&manager);
            thread::Builder::new()
                .name(OPERATIONS_THREAD_NAME.to_owned())
                .spawn(move || serve_orders(&taking, &holding, &sender))
                .map_err(|source| EngineError::Thread { source })?
        };
        Ok(EngineBridge {
            manager: Some(manager),
            events,
            orders,
            forwarding: Some(forwarding),
            operations: Some(operations),
        })
    }

    /// Everything the engine has said since the last look, in the order it
    /// said it.
    ///
    /// The call the window makes in each update cycle. It never waits, and an
    /// engine that has said nothing answers with nothing.
    #[must_use]
    pub fn drain(&self) -> Vec<EngineEvent> {
        let mut said = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => said.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return said,
            }
        }
    }

    /// Read at most one ready event so a UI can enforce its own per-update budget.
    #[must_use]
    pub fn poll(&self) -> Option<EngineEvent> {
        self.events.try_recv().ok()
    }

    /// Begins holding a host, and connecting to it.
    ///
    /// Returns at once: the connecting is the host's own task's business, and
    /// what becomes of it arrives on the channel as the manager says it.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn add_host(&self, alias: &str) -> Result<(), EngineError> {
        self.engine()?.add_host(alias);
        Ok(())
    }

    /// Stops holding a host, and forgets what this client knew about it.
    ///
    /// An order rather than a call, because the call waits for the host's task
    /// to end: what it did arrives as an [`EngineEvent::Finished`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn remove_host(&self, alias: &str) -> Result<(), EngineError> {
        self.order(Operation::Remove {
            host: HostId(alias.to_owned()),
        })
    }

    /// Drops a host's link and opens another at once, without waiting out its
    /// backoff.
    ///
    /// Returns at once; the reconnection arrives as a state the manager says.
    ///
    /// # Errors
    ///
    /// [`EngineError::Manager`] with the manager's own refusal — a host it does
    /// not hold, or one whose task has already ended — and
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn reconnect(&self, alias: &str) -> Result<(), EngineError> {
        self.engine()?
            .reconnect(alias)
            .map_err(EngineError::Manager)
    }

    /// Replaces the server on a host with the one this build carries.
    ///
    /// An order rather than a call: the call stops the host's task first, which
    /// is the one thing that may not happen on the thread that draws frames.
    /// What it did arrives as an [`EngineEvent::Finished`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn upgrade(&self, alias: &str, force: bool) -> Result<(), EngineError> {
        self.order(Operation::Upgrade {
            host: HostId(alias.to_owned()),
            force,
        })
    }

    /// Takes iznik off a host, and stops holding it.
    ///
    /// An order rather than a call, for the reason [`EngineBridge::upgrade`]
    /// is: the call waits for the host's task, and then for a bootstrap.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn uninstall(&self, alias: &str) -> Result<(), EngineError> {
        self.order(Operation::Uninstall {
            host: HostId(alias.to_owned()),
        })
    }

    /// Sends a session command, showing what it does when what it does is
    /// beyond doubt.
    ///
    /// Returns this client's number for the command at once; the answer, and
    /// the change it caused, arrive on the channel as the manager says them.
    ///
    /// # Errors
    ///
    /// As [`EngineBridge::reconnect`].
    pub fn command(&self, alias: &str, command: SessionCommand) -> Result<Submission, EngineError> {
        self.engine()?
            .command(alias, command)
            .map_err(EngineError::Manager)
    }

    /// Subscribe to output; the first screen initializes the application emulator.
    ///
    /// # Errors
    /// Returns the manager's host or channel refusal, or `Stopped` on shutdown.
    pub fn subscribe(&self, alias: &str, pane: PaneId) -> Result<(), EngineError> {
        self.engine()?
            .subscribe(alias, pane)
            .map_err(EngineError::Manager)
    }

    /// Stop carrying a pane when its surface leaves the visible tab.
    ///
    /// # Errors
    /// Returns the manager's host or channel refusal, or `Stopped` on shutdown.
    pub fn unsubscribe(&self, alias: &str, pane: PaneId) -> Result<(), EngineError> {
        self.engine()?
            .unsubscribe(alias, pane)
            .map_err(EngineError::Manager)
    }

    /// Tell the host scheduler which pane has keyboard or pointer focus.
    ///
    /// # Errors
    /// Returns the manager's host or channel refusal, or `Stopped` on shutdown.
    pub fn focus(&self, alias: &str, pane: Option<PaneId>) -> Result<(), EngineError> {
        self.engine()?
            .focus(alias, pane)
            .map_err(EngineError::Manager)
    }

    /// Send encoded input or emulator query replies without waiting on the host.
    ///
    /// # Errors
    /// Returns the manager's host or channel refusal, or `Stopped` on shutdown.
    pub fn input(&self, alias: &str, pane: PaneId, bytes: Vec<u8>) -> Result<(), EngineError> {
        self.engine()?
            .input(alias, pane, bytes)
            .map_err(EngineError::Manager)
    }

    /// Return credit only after the grid consumes the corresponding snapshot.
    ///
    /// # Errors
    /// Returns the manager's host or channel refusal, or `Stopped` on shutdown.
    pub fn credit(&self, alias: &str, pane: PaneId, bytes: u32) -> Result<(), EngineError> {
        self.engine()?
            .credit(alias, pane, bytes)
            .map_err(EngineError::Manager)
    }

    /// Resize the remote pseudoterminal to the grid's measured cell dimensions.
    ///
    /// # Errors
    /// Returns the manager's host or channel refusal, or `Stopped` on shutdown.
    pub fn resize(
        &self,
        alias: &str,
        pane: PaneId,
        columns: u16,
        rows: u16,
    ) -> Result<(), EngineError> {
        self.engine()?
            .resize(alias, pane, columns, rows)
            .map_err(EngineError::Manager)
    }

    /// Route output from the subscription path into the owning emulator thread.
    /// Other model and lifecycle events remain available to the UI mirror.
    ///
    /// # Errors
    /// Returns `Stopped` when the emulator thread has ended.
    pub fn feed_terminal(
        thread: &VtThread,
        event: &ManagerEvent,
        theme: &TerminalTheme,
    ) -> Result<(), VtError> {
        use crate::vt::PaneKey;
        let command = match event {
            ManagerEvent::Screen {
                host,
                pane,
                sequence,
                columns,
                rows,
                bytes,
            } => VtCommand::Screen {
                key: PaneKey {
                    host: host.clone(),
                    pane: *pane,
                },
                sequence: *sequence,
                columns: *columns,
                rows: *rows,
                bytes: bytes.clone(),
                theme: Box::new(theme.clone()),
            },
            ManagerEvent::Bytes {
                host,
                pane,
                sequence,
                bytes,
                receipt,
            } => VtCommand::Feed {
                key: PaneKey {
                    host: host.clone(),
                    pane: *pane,
                },
                sequence: *sequence,
                bytes: bytes.clone(),
                receipt: receipt.clone(),
            },
            // A transport detachment can be followed by Resume, so its emulator
            // must survive; the window closes it when the pane itself is removed.
            _ => return Ok(()),
        };
        thread.send(command)
    }

    /// Return exactly the bytes accepted by one grid, retaining its grant on failure.
    ///
    /// # Errors
    /// Returns the manager's credit-submission failure or a stopped engine.
    pub fn flush_terminal_credit(
        &self,
        grid: &mut crate::grid::TerminalGrid,
    ) -> Result<(), EngineError> {
        grid.flush_receipts(|receipt| {
            self.engine()?
                .credit_receipt(receipt)
                .map_err(EngineError::Manager)
        })?;
        grid.flush_credit(|key, bytes| self.credit(&key.host.0, key.pane, bytes))
    }

    /// Forward emulator answers immediately, or request an authoritative screen
    /// after a gap. The returned snapshot still carries credit for the grid.
    ///
    /// # Errors
    /// Returns engine failures; terminal failures other than gaps remain in the event.
    pub fn terminal_event(
        &self,
        event: VtEvent,
    ) -> Result<Result<Option<VtOutput>, VtError>, EngineError> {
        let alias = &event.key.host.0;
        let pane = event.key.pane;
        match event.result {
            Ok(Some(VtOutput::Snapshot(mut snapshot))) => {
                if !snapshot.responses.is_empty() {
                    self.input(alias, pane, std::mem::take(&mut snapshot.responses))?;
                }
                Ok(Ok(Some(VtOutput::Snapshot(snapshot))))
            }
            Ok(Some(VtOutput::Input(bytes))) => {
                if !bytes.is_empty() {
                    self.input(alias, pane, bytes)?;
                }
                Ok(Ok(None))
            }
            Err(VtError::NeedsScreen) => {
                self.engine()?
                    .screen(alias, pane)
                    .map_err(EngineError::Manager)?;
                Ok(Err(VtError::NeedsScreen))
            }
            result => Ok(result),
        }
    }

    /// The manager, while the engine is running.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when it is not.
    fn engine(&self) -> Result<&HostManager, EngineError> {
        self.manager.as_deref().ok_or(EngineError::Stopped)
    }

    /// Hands one operation to the thread that performs them.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when that thread has gone, which is an engine
    /// that is ending: an order taken and never performed would be a window
    /// showing something nobody was ever asked about.
    fn order(&self, operation: Operation) -> Result<(), EngineError> {
        self.orders
            .send(Some(operation))
            .map_err(|_nowhere| EngineError::Stopped)
    }
}

impl Drop for EngineBridge {
    fn drop(&mut self) {
        // Nothing more will be asked for. The operations thread holds a handle
        // on the manager of its own, so what the join below waits for is the
        // operation running now — never a task of the manager's, which is what
        // the rule at the top of this file is about.
        let _gone = self.orders.send(None);
        if let Some(operations) = self.operations.take() {
            let _joined = operations.join();
        }
        // The last handle, and with it the manager: dropping it ends the
        // manager's runtime and closes the stream the forwarding thread reads,
        // which is what lets that thread end.
        drop(self.manager.take());
        if let Some(forwarding) = self.forwarding.take() {
            let _joined = forwarding.join();
        }
    }
}

/// Reads everything the manager says and puts it on the window's channel.
///
/// It ends when the manager does, or when the window has stopped reading and
/// there is nowhere left to put what is said.
fn relay(events: &Receiver<ManagerEvent>, sender: &Sender<EngineEvent>) {
    while let Ok(event) = events.recv() {
        if sender.send(EngineEvent::Said(event)).is_err() {
            return;
        }
    }
}

/// Serves the orders whose calls wait on the engine's own tasks.
///
/// This is the thread the one rule is about, and it is the only caller of the
/// three waiting methods in this crate. `None` on the channel is what ends it:
/// leaving drops its handle on the manager, and when the bridge has let go of
/// its own, dropping the manager is what ends the runtime and the event
/// stream.
fn serve_orders(
    orders: &Receiver<Option<Operation>>,
    manager: &Arc<HostManager>,
    sender: &Sender<EngineEvent>,
) {
    while let Ok(Some(operation)) = orders.recv() {
        let answer = match &operation {
            Operation::Remove { host } => manager.remove_host(&host.0),
            Operation::Upgrade { host, force } => manager.upgrade(&host.0, *force),
            Operation::Uninstall { host } => manager.uninstall(&host.0),
        };
        if sender
            .send(EngineEvent::Finished { operation, answer })
            .is_err()
        {
            return;
        }
    }
}
