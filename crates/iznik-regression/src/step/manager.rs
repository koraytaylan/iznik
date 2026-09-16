//! The `manager` step: the whole client engine, against real hosts.
//!
//! This is the step the plan's last workstream is for. It stands a
//! `HostManager` up inside the engine container and drives it against the
//! fixture's hosts over real SSH — bootstrapping them from nothing, typing
//! into panes, cutting a link and watching a pane carry on from the byte it
//! held. Everything below it has been proven a layer at a time; what is proven
//! here is that the layers are one thing.
//!
//! Every timing the manager waits on is a field of the step's table, which is
//! why a scenario about a link that drops and comes back finishes in seconds
//! rather than in the minute the product's own defaults would take.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use iznik_client::bootstrap::launch::BootstrapOptions;
use iznik_client::host::identity::{GlobalPaneId, HostId};
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::BackoffPolicy;
use iznik_client::transport::ClientRuntimePaths;
use iznik_client::transport::channel::ChannelOptions;
use iznik_harness::process::{self, Deadline, Output};
use iznik_protocol::identity::PaneId;
use iznik_testkit::vt::Vt;
use serde::Deserialize;

use crate::step::{Context, Outcome, StepError};

/// Where the staged distribution tree is mounted in the containers.
const STAGED_DISTRIBUTION: &str = "/iznik/distribution";

/// The width a pane is made at when the step does not say.
const DEFAULT_COLUMNS: u16 = 80;

/// Its height.
const DEFAULT_ROWS: u16 = 24;

/// How long one wait may take when the step does not say otherwise.
const DEFAULT_PATIENCE: Duration = Duration::from_secs(30);

/// How long to sleep between looks while waiting for something.
const LOOK_INTERVAL: Duration = Duration::from_millis(20);

/// How long sending a signal to a host's daemon may take.
const SIGNAL_DEADLINE: Duration = Duration::from_secs(20);

/// The `[steps.manager]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// Where this build's artifacts are in the container.
    #[serde(default)]
    artifacts: Option<PathBuf>,
    /// How often a channel asks whether its host is there.
    #[serde(default)]
    ping_interval_milliseconds: Option<u64>,
    /// How long silence may last before a link is dead.
    #[serde(default)]
    pong_deadline_milliseconds: Option<u64>,
    /// How long getting a link may take.
    #[serde(default)]
    open_deadline_milliseconds: Option<u64>,
    /// How long a server that is there has to greet.
    #[serde(default)]
    greeting_deadline_milliseconds: Option<u64>,
    /// The wait after a first failure.
    #[serde(default)]
    backoff_initial_milliseconds: Option<u64>,
    /// The longest wait.
    #[serde(default)]
    backoff_maximum_milliseconds: Option<u64>,
    /// How long a command may go unanswered.
    #[serde(default)]
    pending_command_timeout_milliseconds: Option<u64>,
    /// How often unanswered commands are given up on.
    #[serde(default)]
    expire_interval_milliseconds: Option<u64>,
    /// How long one whole bootstrap may take.
    #[serde(default)]
    bootstrap_deadline_seconds: Option<u64>,
    /// How long any one wait in `actions` may take.
    #[serde(default)]
    patience_milliseconds: Option<u64>,
    /// What to do, in order.
    #[serde(default)]
    actions: Vec<Action>,
}

/// One thing a manager step does.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    /// Begin holding a host, and connecting to it.
    AddHost {
        /// The alias, as `~/.ssh/config` names it.
        alias: String,
    },
    /// Wait until a host's state reads as this.
    AwaitState {
        /// The host.
        alias: String,
        /// What its state must begin with.
        is: String,
    },
    /// Make a session, and with it a tab and a pane.
    CreateSession {
        /// The host.
        alias: String,
        /// What to call it.
        name: String,
        /// The pane's width.
        #[serde(default)]
        columns: Option<u16>,
        /// Its height.
        #[serde(default)]
        rows: Option<u16>,
    },
    /// Rename a session, which shows before the host answers.
    RenameSession {
        /// The host.
        alias: String,
        /// Which session.
        session: u64,
        /// What to call it.
        name: String,
    },
    /// Wait until a host's model has reached a generation.
    AwaitDelta {
        /// The host.
        alias: String,
        /// The generation it must have reached.
        generation: u64,
    },
    /// Begin delivery of a pane's output.
    Subscribe {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
    },
    /// Send keystrokes.
    Input {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// What to type.
        text: String,
    },
    /// Tell a pane it is another size.
    Resize {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// Its width.
        columns: u16,
        /// Its height.
        rows: u16,
    },
    /// Say which pane the person is looking at.
    Focus {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
    },
    /// Read until a pane's bytes hold something.
    AwaitBytes {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// What they must hold.
        contains: String,
        /// How long this one wait may take, when it is shorter than the
        /// step's own patience — which is how a case about one host staying
        /// quick while another is ill says what quick means.
        #[serde(default)]
        within_milliseconds: Option<u64>,
    },
    /// Write everything received for a pane to a file.
    CaptureTo {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// Where to write it.
        path: PathBuf,
    },
    /// Ask for a pane's screen and write it to a file.
    ScreenTo {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// Where to write it.
        path: PathBuf,
    },
    /// Say what a pane is called everywhere, and hold it to that.
    ExpectAddress {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// What its address must be.
        address: String,
    },
    /// Replace the server on a host.
    Upgrade {
        /// The host.
        alias: String,
        /// Whether it may end the panes the host is holding.
        #[serde(default)]
        force: bool,
        /// Words the refusal must carry, when one is expected.
        #[serde(default)]
        refused: Option<String>,
    },
    /// Take iznik off a host.
    Uninstall {
        /// The host.
        alias: String,
    },
    /// Stop the daemon on a host where it stands, so that the link stays up
    /// and nothing answers on it.
    ///
    /// What a network fault would do to a channel is what this does, and it is
    /// what a scenario can do from inside one step: a manager cannot outlive
    /// the process that made it, so a fault applied between steps would be
    /// met by a manager that had not been born when the link was cut. Neither
    /// container carries the traffic tooling to cut a network from inside one.
    PauseHost {
        /// The host.
        alias: String,
    },
    /// Let it go again.
    ResumeHost {
        /// The host.
        alias: String,
    },
    /// Feed the pieces into one emulator and the pane's own screen into
    /// another, and fail on a difference.
    ExpectReassembly {
        /// The host.
        alias: String,
        /// The pane.
        pane: u64,
        /// The files, in the order they are fed.
        pieces: Vec<PathBuf>,
    },
}

/// What has been heard about one pane.
#[derive(Debug, Default)]
struct Heard {
    /// Every byte that has arrived for it, in order.
    bytes: Vec<u8>,
    /// The byte position the first of them is.
    from: Option<u64>,
    /// The last screen the host sent for it.
    screen: Option<Vec<u8>>,
    /// The byte position that screen is exact at.
    at: Option<u64>,
    /// The size it was sent at.
    size: Option<(u16, u16)>,
    /// Where the last screen written to a file was exact at, which is where
    /// the bytes that follow it begin.
    marked: Option<u64>,
}

impl Heard {
    /// Takes what arrived at a position in the stream.
    ///
    /// What is held has to run from `from` without a hole in it, because that
    /// is the only reason a position can be turned into an offset. A cursor
    /// does move: a host that answers a resume with a screen puts the
    /// subscription where the screen is exact, and the bytes after it begin
    /// there rather than where the last ones ended. What was held before such
    /// a jump belongs to a stream this no longer has all of, so it is let go
    /// and the new position is where this begins.
    fn took(&mut self, sequence: u64, bytes: &[u8]) {
        let held = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        let next = self.from.map(|first| first.saturating_add(held));
        if next == Some(sequence) {
            self.bytes.extend_from_slice(bytes);
            return;
        }
        self.from = Some(sequence);
        self.bytes = bytes.to_vec();
    }

    /// The bytes between two positions in the stream, as far as they are held.
    ///
    /// A screen is exact at a position and the capture begins at another, so
    /// feeding a screen and then every byte held would show what the screen
    /// already showed a second time. This is the window between them.
    ///
    /// Nothing when the window begins before what is held: answering with
    /// what there is would be answering a different question, and a case that
    /// compares an empty window fails saying so.
    fn between(&self, first: Option<u64>, last: Option<u64>) -> &[u8] {
        let Some(from) = self.from else {
            return &[];
        };
        if first.is_some_and(|named| named < from) {
            return &[];
        }
        let held = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        let start = first.unwrap_or(from).saturating_sub(from);
        let end = last
            .map_or(held, |named| named.saturating_sub(from))
            .min(held);
        let (start, end) = (
            usize::try_from(start.min(end)).unwrap_or(0),
            usize::try_from(end).unwrap_or(self.bytes.len()),
        );
        self.bytes.get(start..end).unwrap_or(&[])
    }
}

/// Everything the step has heard, as it heard it.
#[derive(Debug, Default)]
struct Watched {
    /// The last state each host reported, as it reads.
    states: BTreeMap<HostId, String>,
    /// What has been heard about each pane.
    panes: BTreeMap<(HostId, PaneId), Heard>,
}

impl Watched {
    /// Takes in one event.
    fn take(&mut self, event: ManagerEvent) {
        match event {
            ManagerEvent::Moved { host, state } => {
                let _before = self.states.insert(host, state.to_string());
            }
            ManagerEvent::Bytes {
                host,
                pane,
                sequence,
                bytes,
                ..
            } => {
                let held = self.panes.entry((host, pane)).or_default();
                held.took(sequence.0, &bytes);
            }
            ManagerEvent::Screen {
                host,
                pane,
                sequence,
                columns,
                rows,
                bytes,
            } => {
                let held = self.panes.entry((host, pane)).or_default();
                held.screen = Some(bytes);
                held.at = Some(sequence.0);
                held.size = Some((columns, rows));
            }
            ManagerEvent::Notify(_)
            | ManagerEvent::Removed { .. }
            | ManagerEvent::Detached { .. }
            | ManagerEvent::Snapshot { .. }
            | ManagerEvent::Delta { .. } => {}
        }
    }

    /// What has been heard about one pane.
    fn pane(&self, alias: &str, pane: u64) -> Option<&Heard> {
        self.panes.get(&(HostId(alias.to_owned()), PaneId(pane)))
    }

    /// Says that the screen a pane last sent has been written down, so that
    /// what follows it is the bytes from there on.
    fn mark_screen(&mut self, alias: &str, pane: u64) {
        if let Some(held) = self
            .panes
            .get_mut(&(HostId(alias.to_owned()), PaneId(pane)))
        {
            held.marked = held.at;
        }
    }

    /// Forgets the screen held for a pane.
    ///
    /// Asked before one is requested, because a host sends a screen when a
    /// pane is first subscribed to as well as when it is asked for: waiting
    /// for "a screen" without forgetting that one would be waiting for
    /// something that has already arrived, and comparing against a picture of
    /// the pane as it was before anything was typed into it.
    fn forget_screen(&mut self, alias: &str, pane: u64) {
        if let Some(held) = self
            .panes
            .get_mut(&(HostId(alias.to_owned()), PaneId(pane)))
        {
            held.screen = None;
        }
    }
}

/// The options the manager runs under, as the step's table says.
///
/// # Errors
///
/// The step's own words when the client's runtime paths cannot be made.
fn options(body: &Body) -> Result<ManagerOptions, String> {
    let paths = ClientRuntimePaths::resolve().map_err(|error| error.to_string())?;
    let artifacts = body
        .artifacts
        .clone()
        .unwrap_or_else(|| PathBuf::from(STAGED_DISTRIBUTION));
    let mut held = ManagerOptions::new(artifacts, paths);
    let channel = ChannelOptions::default();
    held.channel = ChannelOptions {
        ping_interval: milliseconds(body.ping_interval_milliseconds, channel.ping_interval),
        pong_deadline: milliseconds(body.pong_deadline_milliseconds, channel.pong_deadline),
        open_deadline: milliseconds(body.open_deadline_milliseconds, channel.open_deadline),
        greeting_deadline: milliseconds(
            body.greeting_deadline_milliseconds,
            channel.greeting_deadline,
        ),
    };
    let backoff = BackoffPolicy::default();
    held.backoff = BackoffPolicy {
        initial: milliseconds(body.backoff_initial_milliseconds, backoff.initial),
        maximum: milliseconds(body.backoff_maximum_milliseconds, backoff.maximum),
        ..backoff
    };
    held.pending_command_timeout = milliseconds(
        body.pending_command_timeout_milliseconds,
        held.pending_command_timeout,
    );
    held.expire_interval = milliseconds(body.expire_interval_milliseconds, held.expire_interval);
    if let Some(seconds) = body.bootstrap_deadline_seconds {
        held.bootstrap_deadline = Duration::from_secs(seconds);
    }
    held.bootstrap = BootstrapOptions {
        channel: held.channel.clone(),
        ..BootstrapOptions::default()
    };
    Ok(held)
}

/// A duration a field says, or the one it falls back to.
fn milliseconds(said: Option<u64>, otherwise: Duration) -> Duration {
    said.map_or(otherwise, Duration::from_millis)
}

/// Takes in everything that has arrived without waiting for more.
fn drain(events: &Receiver<ManagerEvent>, watched: &mut Watched) {
    while let Ok(event) = events.try_recv() {
        watched.take(event);
    }
}

/// Waits until what has been heard satisfies `wanted`.
///
/// # Errors
///
/// The step's own words when it never does.
fn await_until(
    events: &Receiver<ManagerEvent>,
    watched: &mut Watched,
    deadline: Instant,
    what: &str,
    wanted: impl Fn(&Watched) -> bool,
) -> Result<(), String> {
    while Instant::now() < deadline {
        drain(events, watched);
        if wanted(watched) {
            return Ok(());
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    drain(events, watched);
    if wanted(watched) {
        return Ok(());
    }
    Err(format!("{what} never happened"))
}

/// Whether a byte string holds another.
fn holds(held: &[u8], wanted: &[u8]) -> bool {
    !wanted.is_empty() && held.windows(wanted.len()).any(|piece| piece == wanted)
}

/// Runs every action against a manager of this step's own.
///
/// # Errors
///
/// The step's own words when an action does not do what it says.
fn drive(body: &Body, deadline: Instant) -> Result<String, String> {
    let manager = HostManager::new(options(body)?).map_err(|error| error.to_string())?;
    let events = manager.events();
    let mut watched = Watched::default();
    let patience = milliseconds(body.patience_milliseconds, DEFAULT_PATIENCE);
    for action in &body.actions {
        let until = Instant::now()
            .checked_add(patience)
            .unwrap_or(deadline)
            .min(deadline);
        act(&manager, &events, &mut watched, action, until)?;
    }
    Ok(format!("{} actions", body.actions.len()))
}

/// Does one action.
///
/// # Errors
///
/// The step's own words when it does not do what it says.
fn act(
    manager: &HostManager,
    events: &Receiver<ManagerEvent>,
    watched: &mut Watched,
    action: &Action,
    deadline: Instant,
) -> Result<(), String> {
    drain(events, watched);
    match action {
        Action::Subscribe { .. }
        | Action::Input { .. }
        | Action::Resize { .. }
        | Action::Focus { .. }
        | Action::AwaitBytes { .. }
        | Action::CaptureTo { .. }
        | Action::ScreenTo { .. }
        | Action::ExpectAddress { .. }
        | Action::ExpectReassembly { .. } => {
            about_a_pane(manager, events, watched, action, deadline)
        }
        _about_a_host => about_a_host(manager, events, watched, action, deadline),
    }
}

/// The actions that are about a host rather than about one of its panes.
///
/// # Errors
///
/// The step's own words when one of them does not do what it says.
fn about_a_host(
    manager: &HostManager,
    events: &Receiver<ManagerEvent>,
    watched: &mut Watched,
    action: &Action,
    deadline: Instant,
) -> Result<(), String> {
    match action {
        Action::AddHost { alias } => {
            manager.add_host(alias);
            Ok(())
        }
        Action::AwaitState { alias, is } => {
            let named = HostId(alias.clone());
            await_until(
                events,
                watched,
                deadline,
                &format!("{alias} reading as {is:?}"),
                |held| {
                    held.states
                        .get(&named)
                        .is_some_and(|state| state.starts_with(is.as_str()))
                },
            )
        }
        Action::CreateSession {
            alias,
            name,
            columns,
            rows,
        } => {
            let asked = iznik_protocol::command::SessionCommand::CreateSession {
                name: name.clone(),
                columns: columns.unwrap_or(DEFAULT_COLUMNS),
                rows: rows.unwrap_or(DEFAULT_ROWS),
                working_directory: None,
            };
            let _sent = manager
                .command(alias, asked)
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        Action::RenameSession {
            alias,
            session,
            name,
        } => {
            let asked = iznik_protocol::command::SessionCommand::RenameSession {
                session: iznik_protocol::identity::SessionId(*session),
                name: name.clone(),
            };
            let _sent = manager
                .command(alias, asked)
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        Action::AwaitDelta { alias, generation } => {
            let named = HostId(alias.clone());
            let wanted = *generation;
            let reached = || {
                manager
                    .model()
                    .host(&named)
                    .is_some_and(|view| view.model.generation.0 >= wanted)
            };
            await_until(
                events,
                watched,
                deadline,
                &format!("{alias} reaching generation {wanted}"),
                |_held| reached(),
            )
        }
        Action::Upgrade {
            alias,
            force,
            refused,
        } => upgraded(manager, alias, *force, refused.as_deref()),
        Action::Uninstall { alias } => manager.uninstall(alias).map_err(|error| error.to_string()),
        Action::PauseHost { alias } => signal(alias, "STOP"),
        Action::ResumeHost { alias } => signal(alias, "CONT"),
        // Every other action is about a pane, and `act` has sent it there.
        _about_a_pane => Ok(()),
    }
}

/// The actions that are about one pane.
///
/// # Errors
///
/// The step's own words when one of them does not do what it says.
fn about_a_pane(
    manager: &HostManager,
    events: &Receiver<ManagerEvent>,
    watched: &mut Watched,
    action: &Action,
    deadline: Instant,
) -> Result<(), String> {
    match action {
        Action::Subscribe { alias, pane } => manager
            .subscribe(alias, PaneId(*pane))
            .map_err(|error| error.to_string()),
        Action::Input { alias, pane, text } => manager
            .input(alias, PaneId(*pane), text.clone().into_bytes())
            .map_err(|error| error.to_string()),
        Action::Resize {
            alias,
            pane,
            columns,
            rows,
        } => manager
            .resize(alias, PaneId(*pane), *columns, *rows)
            .map_err(|error| error.to_string()),
        Action::Focus { alias, pane } => manager
            .focus(alias, Some(PaneId(*pane)))
            .map_err(|error| error.to_string()),
        Action::AwaitBytes {
            alias,
            pane,
            contains,
            within_milliseconds,
        } => {
            let until = within_milliseconds.map_or(deadline, |named| {
                let now = Instant::now();
                now.checked_add(Duration::from_millis(named))
                    .unwrap_or(now)
                    .min(deadline)
            });
            await_until(
                events,
                watched,
                until,
                &format!("pane {pane} on {alias} saying {contains:?}"),
                |held| {
                    held.pane(alias, *pane)
                        .is_some_and(|heard| holds(&heard.bytes, contains.as_bytes()))
                },
            )
        }
        Action::CaptureTo { alias, pane, path } => {
            let heard = watched
                .pane(alias, *pane)
                .ok_or_else(|| format!("nothing was heard for pane {pane} on {alias}"))?;
            std::fs::write(path, &heard.bytes)
                .map_err(|error| format!("{}: {error}", path.display()))
        }
        Action::ScreenTo { alias, pane, path } => {
            watched.forget_screen(alias, *pane);
            manager
                .screen(alias, PaneId(*pane))
                .map_err(|error| error.to_string())?;
            await_until(
                events,
                watched,
                deadline,
                &format!("a screen for pane {pane} on {alias}"),
                |held| {
                    held.pane(alias, *pane)
                        .is_some_and(|heard| heard.screen.is_some())
                },
            )?;
            let heard = watched
                .pane(alias, *pane)
                .and_then(|held| held.screen.clone())
                .ok_or_else(|| format!("no screen for pane {pane} on {alias}"))?;
            std::fs::write(path, &heard).map_err(|error| format!("{}: {error}", path.display()))?;
            // What follows this screen in the stream is what a reassembly
            // built on it must be fed, and nothing before it.
            watched.mark_screen(alias, *pane);
            Ok(())
        }
        Action::ExpectAddress {
            alias,
            pane,
            address,
        } => addressed(manager, alias, *pane, address),
        Action::ExpectReassembly {
            alias,
            pane,
            pieces,
        } => reassembled(manager, events, watched, alias, *pane, pieces, deadline),
        // Every other action is about a host, and `act` has sent it there.
        _about_a_host => Ok(()),
    }
}

/// The shell that finds the daemon's lock wherever the host put it and sends
/// it a signal.
///
/// The same two places `RuntimePaths::resolve` looks, in the same order.
fn signal_the_daemon(named: &str) -> String {
    format!(
        "if [ -n \"$XDG_RUNTIME_DIR\" ]; then lock=\"$XDG_RUNTIME_DIR/iznik/server.lock\"; \
         else lock=\"${{TMPDIR:-/tmp}}/iznik-$(id -u)/server.lock\"; fi; \
         kill -{named} \"$(cat \"$lock\")\""
    )
}

/// Sends a signal to the daemon on a host.
///
/// # Errors
///
/// The step's own words when `ssh` will not run or the signal is refused.
fn signal(alias: &str, named: &str) -> Result<(), String> {
    let mut command = Command::new("ssh");
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(alias)
        .arg(signal_the_daemon(named));
    process::run(command, Deadline(SIGNAL_DEADLINE), Output::Capture)
        .map(|_said| ())
        .map_err(|error| error.to_string())
}

/// Holds a pane the client really has to the address it is known by
/// everywhere.
///
/// The model is asked first, because an address rendered from the action's own
/// words and compared with the action's own words says nothing about whether
/// the pane is there.
///
/// # Errors
///
/// The step's own words when the host holds no such pane, or its address is
/// not the one named.
fn addressed(manager: &HostManager, alias: &str, pane: u64, address: &str) -> Result<(), String> {
    let host = HostId(alias.to_owned());
    let held = manager.model();
    let view = held
        .host(&host)
        .ok_or_else(|| format!("{alias} is not held"))?;
    let holds = view
        .model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .any(|found| found.id == PaneId(pane));
    if !holds {
        return Err(format!("{alias} holds no pane {pane}"));
    }
    if view.subscription(PaneId(pane)).is_none() {
        return Err(format!("pane {pane} on {alias} is not subscribed to"));
    }
    let named = GlobalPaneId {
        host,
        pane: PaneId(pane),
    };
    if named.to_string() == address {
        return Ok(());
    }
    Err(format!("the pane is {named} and not {address}"))
}

/// Replaces a host's server, or holds it to the refusal that was expected.
///
/// # Errors
///
/// The step's own words when it succeeds where a refusal was wanted, or
/// refuses with something else.
fn upgraded(
    manager: &HostManager,
    alias: &str,
    force: bool,
    refused: Option<&str>,
) -> Result<(), String> {
    match manager.upgrade(alias, force) {
        Ok(()) => match refused {
            Some(words) => Err(format!(
                "expected a refusal saying {words:?} and it upgraded"
            )),
            None => Ok(()),
        },
        Err(error) => {
            let said = error.to_string();
            let Some(words) = refused else {
                return Err(said);
            };
            if said.contains(words) {
                return Ok(());
            }
            Err(format!(
                "expected a refusal saying {words:?} and got {said}"
            ))
        }
    }
}

/// Feeds the pieces into one emulator and the pane's own screen into another,
/// and says whether they show the same thing.
///
/// # Errors
///
/// The step's own words when a piece cannot be read, the emulator refuses, or
/// the two differ.
fn reassembled(
    manager: &HostManager,
    events: &Receiver<ManagerEvent>,
    watched: &mut Watched,
    alias: &str,
    pane: u64,
    pieces: &[PathBuf],
    deadline: Instant,
) -> Result<(), String> {
    watched.forget_screen(alias, pane);
    manager
        .screen(alias, PaneId(pane))
        .map_err(|error| error.to_string())?;
    await_until(
        events,
        watched,
        deadline,
        &format!("a screen for pane {pane} on {alias}"),
        |held| {
            held.pane(alias, pane)
                .is_some_and(|heard| heard.screen.is_some())
        },
    )?;
    let heard = watched
        .pane(alias, pane)
        .ok_or_else(|| format!("nothing was heard for pane {pane} on {alias}"))?;
    let (columns, rows) = heard.size.unwrap_or((DEFAULT_COLUMNS, DEFAULT_ROWS));
    let truth = heard
        .screen
        .as_ref()
        .ok_or_else(|| format!("no screen for pane {pane} on {alias}"))?;
    let mut reassembled = Vt::new(columns, rows).map_err(|error| error.to_string())?;
    for piece in pieces {
        let bytes =
            std::fs::read(piece).map_err(|error| format!("{}: {error}", piece.display()))?;
        reassembled.feed(&bytes);
    }
    // And then the pane's own bytes, from where those pieces left off to
    // where the screen this is compared against is exact — taken here rather
    // than captured earlier, so nothing said in between is in one and not the
    // other.
    reassembled.feed(heard.between(heard.marked, heard.at));
    let mut mirrored = Vt::new(columns, rows).map_err(|error| error.to_string())?;
    mirrored.feed(truth);
    let shown = reassembled.snapshot().map_err(|error| error.to_string())?;
    let expected = mirrored.snapshot().map_err(|error| error.to_string())?;
    if shown == expected {
        return Ok(());
    }
    Err(format!(
        "the pieces reassemble to a different screen than the pane's own:\n{shown}\n---\n{expected}"
    ))
}

/// The `manager` step. Its body is the `[steps.manager]` table.
///
/// # Errors
///
/// [`StepError::Malformed`] when the body is not the table this expects.
pub fn execute(
    _context: &Context,
    body: &toml::Value,
    timeout: Duration,
) -> Result<Outcome, StepError> {
    let asked: Body =
        body.clone()
            .try_into()
            .map_err(|error: toml::de::Error| StepError::Malformed {
                detail: format!("a `manager` step: {error}"),
            })?;
    let started = Instant::now();
    let deadline = started.checked_add(timeout).unwrap_or(started);
    let driven = drive(&asked, deadline);
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
