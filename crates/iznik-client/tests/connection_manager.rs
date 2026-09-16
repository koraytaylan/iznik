//! Several hosts at once, and what one of them being ill does to the others.
//!
//! Every case here stands whole servers up on this machine and reaches them
//! through the `unix:` alias this crate owns, because what is being asked
//! about is the manager and not the network: isolation between hosts, a resume
//! that carries a pane's bytes across a dropped link, and a command that shows
//! before the host has agreed to it. The same over SSH, against containers, is
//! `end-to-end-ssh`.
//!
//! A relay stands between the manager and one daemon so that a link can be cut
//! and let back: nothing else in reach can drop a connection without also
//! taking away the server it led to, and a resume needs the server to still be
//! there.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use iznik_client::host::identity::{GlobalPaneId, HostId};
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::{BackoffPolicy, HostState};
use iznik_client::model::ClientModel;
use iznik_client::reduce::Notification;
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_protocol::command::{CommandOutcome, RejectionCode, SessionCommand};
use iznik_protocol::identity::{PaneId, SessionId};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use tokio::sync::watch;

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long it waits for something that should not happen at all.
const BRIEF: Duration = Duration::from_millis(300);

/// How long a keystroke may take to come back on a healthy host while another
/// host is stuck. Generous, because what is being asked is whether it is
/// waiting on the other host at all, not how fast this machine is.
const LATENCY_BUDGET: Duration = Duration::from_secs(3);

/// How long the whole of a reconnection may take.
const RECONNECT_BUDGET: Duration = Duration::from_secs(2);

/// The backoff these cases run under: tens of milliseconds, so a case about a
/// growing wait takes no longer than one about anything else.
const QUICK_INITIAL: Duration = Duration::from_millis(20);

/// And a ceiling under a second.
const QUICK_MAXIMUM: Duration = Duration::from_millis(200);

/// The least the four waits must differ by for the difference to be the
/// jitter rather than the cost of reading a clock.
const JITTER_FLOOR: Duration = Duration::from_micros(100);

/// The width a pane is made at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// The first pane every one of these makes.
const PANE: PaneId = PaneId(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A temporary directory of this case's own, removed when the guard drops.
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
    let path = base.join(format!("iznik-manager-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the servers these cases stand up.
///
/// The manager owns its own; nothing here may call its methods from inside a
/// runtime, so this one is only ever used to start things and then left to run
/// them on its own threads.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// The manager these cases drive, with an empty artifacts directory: nothing
/// reached through `unix:` is ever installed.
///
/// # Errors
///
/// When the runtime paths cannot be made or the manager cannot be built.
fn manager(held: &Scratch) -> Result<HostManager, Failed> {
    manager_with_backoff(held, QUICK_INITIAL, QUICK_MAXIMUM)
}

/// Construct a manager with explicit retry timing for deadline observations.
///
/// # Errors
/// Returns runtime path, artifact directory or manager initialization errors.
fn manager_with_backoff(
    held: &Scratch,
    initial: Duration,
    maximum: Duration,
) -> Result<HostManager, Failed> {
    let artifacts = held.path.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let mut options = ManagerOptions::new(artifacts, paths);
    options.backoff = BackoffPolicy {
        initial,
        maximum,
        ..BackoffPolicy::default()
    };
    options.channel = ChannelOptions {
        ping_interval: Duration::from_millis(50),
        pong_deadline: Duration::from_millis(400),
        open_deadline: Duration::from_secs(5),
        // A server on this machine greets in microseconds; one that has not in
        // three hundred milliseconds is one of these cases' silent sockets,
        // and waiting five seconds for it would be waiting for nothing.
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

/// A relay in front of a daemon, so a link can be cut and let back.
struct Relay {
    /// The socket the manager connects to.
    socket: PathBuf,
    /// Whether connections through it are being carried. Watched rather than
    /// polled, so that cutting takes effect before the next thing the case
    /// does — a poll would let whatever was written in the meantime through,
    /// and a case about a command nobody answered would have it answered.
    carrying: watch::Sender<bool>,
}

impl Relay {
    /// Stops carrying, and drops whatever is being carried now.
    fn cut(&self) {
        let _told = self.carrying.send(false);
    }

    /// Carries again.
    fn restore(&self) {
        let _told = self.carrying.send(true);
    }
}

/// Waits for the relay to stop carrying.
async fn until_cut(mut watching: watch::Receiver<bool>) {
    while *watching.borrow_and_update() {
        if watching.changed().await.is_err() {
            return;
        }
    }
}

/// Stands a relay in front of `behind`, listening on a socket under `held`.
///
/// # Errors
///
/// When the socket cannot be bound.
fn relay(runtime: &Runtime, held: &Scratch, behind: &std::path::Path) -> Result<Relay, Failed> {
    let socket = held.path.join("relay.sock");
    let listener = runtime.block_on(async { UnixListener::bind(&socket) })?;
    let (carrying, watching) = watch::channel(true);
    let target = behind.to_path_buf();
    let _serving = runtime.spawn(async move {
        loop {
            let Ok((near, _from)) = listener.accept().await else {
                return;
            };
            if !*watching.borrow() {
                continue;
            }
            let Ok(far) = UnixStream::connect(&target).await else {
                continue;
            };
            let alive = watching.clone();
            let _carrying = tokio::spawn(async move {
                let copying = async {
                    let (mut near, mut far) = (near, far);
                    let _both = tokio::io::copy_bidirectional(&mut near, &mut far).await;
                };
                tokio::select! {
                    () = copying => {}
                    () = until_cut(alive) => {}
                }
            });
        }
    });
    Ok(Relay { socket, carrying })
}

/// Waits for an event the predicate accepts, and gives it back.
///
/// # Errors
///
/// When none arrives inside `patience`.
fn await_event(
    events: &Receiver<ManagerEvent>,
    what: &str,
    patience: Duration,
    wanted: impl Fn(&ManagerEvent) -> bool,
) -> Result<ManagerEvent, Failed> {
    let expires = Instant::now().checked_add(patience).ok_or("no clock")?;
    // What went past while waiting, because a case that says only "nothing
    // came" leaves whoever reads it to guess what did.
    let mut passed = Vec::new();
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(event) if wanted(&event) => return Ok(event),
            Ok(other) => passed.push(format!("{other:?}")),
            Err(_nothing) => break,
        }
    }
    Err(format!("no {what} inside {patience:?}; what came was {passed:?}").into())
}

/// Whether an event says a host is connected.
fn is_connected(event: &ManagerEvent) -> bool {
    matches!(
        event,
        ManagerEvent::Moved {
            state: HostState::Connected { .. },
            ..
        }
    )
}

/// Waits for every one of these hosts to be connected, in whatever order they
/// manage it.
///
/// One receiver carries every host's events, so waiting for one host at a time
/// and discarding what did not match would throw away the other's.
///
/// # Errors
///
/// When they are not all connected inside `PROMPT`.
fn await_connected(events: &Receiver<ManagerEvent>, hosts: &[&str]) -> Result<(), Failed> {
    let mut waiting: Vec<HostId> = hosts
        .iter()
        .map(|held| HostId((*held).to_owned()))
        .collect();
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while !waiting.is_empty() && Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if is_connected(&event)
            && let ManagerEvent::Moved { host, .. } = event
        {
            waiting.retain(|held| held != &host);
        }
    }
    if waiting.is_empty() {
        return Ok(());
    }
    Err(format!("{waiting:?} did not connect inside {PROMPT:?}").into())
}

/// Makes a session on a host and waits for the model to hold it.
///
/// # Errors
///
/// When the command is refused or the model never shows it.
fn make_a_session(
    held: &HostManager,
    events: &Receiver<ManagerEvent>,
    host: &str,
    name: &str,
) -> Result<SessionId, Failed> {
    let submission = held.command(
        host,
        SessionCommand::CreateSession {
            name: name.to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        },
    )?;
    let answered = await_event(
        events,
        &format!("an answer to {host}'s session command"),
        PROMPT,
        |event| {
            matches!(
                event,
                ManagerEvent::Notify(Notification::CommandFinished { command, .. })
                    if *command == submission.id
            )
        },
    )?;
    let ManagerEvent::Notify(Notification::CommandFinished { outcome, .. }) = answered else {
        return Err("the answer is a command result".into());
    };
    match outcome {
        CommandOutcome::Applied { .. } => {}
        CommandOutcome::Rejected { message, .. } => {
            return Err(format!("the session was refused: {message}").into());
        }
    }
    // The delta that carries it follows the answer.
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        if let Some(session) = first_session(&held.model(), host) {
            return Ok(session);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("the model never showed the session".into())
}

/// The first session one host holds, if it holds one.
fn first_session(model: &ClientModel, host: &str) -> Option<SessionId> {
    model
        .host(&HostId(host.to_owned()))?
        .model
        .sessions
        .first()
        .map(|held| held.id)
}

/// # Panics
///
/// When two hosts are not two: a session on one showing on the other, or an
/// answer arriving for the wrong one.
#[test]
fn connection_manager_holds_two_hosts_apart() {
    let case = || -> Result<(), Failed> {
        let held = scratch("two")?;
        let runtime = runtime()?;
        let first = runtime.block_on(Stack::start(StackOptions::default()))?;
        let second = runtime.block_on(Stack::start(StackOptions::default()))?;
        let manager = manager(&held)?;
        let events = manager.events();
        let (work, build) = (alias(first.socket()), alias(second.socket()));
        manager.add_host(&work);
        manager.add_host(&build);
        await_connected(&events, &[&work, &build])?;
        let here = make_a_session(&manager, &events, &work, "here")?;
        let there = make_a_session(&manager, &events, &build, "there")?;
        let model = manager.model();
        let named = |host: &str| -> Vec<String> {
            model
                .host(&HostId(host.to_owned()))
                .map(|view| {
                    view.model
                        .sessions
                        .iter()
                        .map(|session| session.name.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(named(&work), vec!["here".to_owned()], "one host's own");
        assert_eq!(named(&build), vec!["there".to_owned()], "and the other's");
        assert_ne!(
            (here, &work),
            (there, &build),
            "and the two sessions are two things"
        );
        drop(manager);
        drop(first);
        drop(second);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When one host being stuck is visible in another.
#[test]
fn connection_manager_keeps_a_stuck_host_to_itself() {
    let case = || -> Result<(), Failed> {
        let held = scratch("stuck")?;
        let runtime = runtime()?;
        let healthy = runtime.block_on(Stack::start(StackOptions::default()))?;
        // A socket that accepts and never says anything: a host whose daemon
        // is stopped where it stands looks exactly like this from here.
        let stuck = held.path.join("stuck.sock");
        let listener = runtime.block_on(async { UnixListener::bind(&stuck) })?;
        let _holding = runtime.spawn(async move {
            let mut kept = Vec::new();
            while let Ok((stream, _from)) = listener.accept().await {
                kept.push(stream);
            }
        });
        let manager = manager(&held)?;
        let events = manager.events();
        let (work, silent) = (alias(healthy.socket()), alias(&stuck));
        manager.add_host(&silent);
        manager.add_host(&work);
        await_connected(&events, &[&work])?;
        let session = make_a_session(&manager, &events, &work, "work")?;
        // Everything about the healthy host must go on being quick while the
        // other one is stuck opening a channel that will never open.
        let started = Instant::now();
        manager.subscribe(&work, PANE)?;
        manager.input(&work, PANE, b"echo apart-$((6*7))\n".to_vec())?;
        let _seen = await_event(&events, "the echo", LATENCY_BUDGET, |event| match event {
            ManagerEvent::Bytes { host, bytes, .. } => {
                host == &HostId(work.clone()) && contains(bytes, b"apart-42")
            }
            _other => false,
        })?;
        let taken = started.elapsed();
        assert!(
            taken < LATENCY_BUDGET,
            "a keystroke on a healthy host comes back in {taken:?}, whatever another host is doing"
        );
        assert!(
            session.0 > 0,
            "and the session it was typed into is the healthy host's"
        );
        drop(manager);
        drop(healthy);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Whether a needle is somewhere in a haystack.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len().max(1))
        .any(|window| window == needle)
}

/// # Panics
///
/// When a link that dropped does not come back, or a pane's bytes start again
/// rather than carrying on.
#[test]
fn connection_manager_resumes_a_pane_where_it_left_off() {
    let case = || -> Result<(), Failed> {
        let held = scratch("resume")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let carried = relay(&runtime, &held, stack.socket())?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(&carried.socket);
        manager.add_host(&host);
        await_connected(&events, &[&host])?;
        let _session = make_a_session(&manager, &events, &host, "work")?;
        manager.subscribe(&host, PANE)?;
        // `sh` prints nothing until it is given something to do, so the first
        // bytes are the ones this asks for.
        manager.input(&host, PANE, b"echo before-$((6*7))\n".to_vec())?;
        let _first = await_event(&events, "the pane's first bytes", PROMPT, |event| {
            matches!(event, ManagerEvent::Bytes { pane, bytes, .. }
                if *pane == PANE && contains(bytes, b"before-42"))
        })?;
        // Where the stream stood before anything went wrong. Sampled now, and
        // not after the reconnection: a cursor read afterwards would be small
        // again if the host had started the pane over, and the case would not
        // know the difference.
        let before = cursor(&manager, &host)?;
        // The link goes, and comes back on its own.
        let started = Instant::now();
        carried.cut();
        let _dead = await_event(&events, "a reconnection", PROMPT, |event| {
            matches!(
                event,
                ManagerEvent::Moved {
                    state: HostState::Reconnecting { .. },
                    ..
                }
            )
        })?;
        carried.restore();
        await_connected(&events, &[&host])?;
        let back = started.elapsed();
        assert!(
            back < RECONNECT_BUDGET,
            "a link that dropped is back in {back:?}"
        );
        // And the pane carries on: what is typed after the drop arrives, and
        // the byte it starts at is past where the client had got to.
        manager.input(&host, PANE, b"echo across-$((6*7))\n".to_vec())?;
        let seen = await_event(
            &events,
            "the echo after the drop",
            PROMPT,
            |event| match event {
                ManagerEvent::Bytes { pane, bytes, .. } => {
                    *pane == PANE && contains(bytes, b"across-42")
                }
                _other => false,
            },
        )?;
        let ManagerEvent::Bytes { sequence, .. } = seen else {
            return Err("the event carries the byte it starts at".into());
        };
        assert!(
            sequence.0 >= before.0,
            "the stream carried on from where it stood before the drop \
             ({before:?}) rather than starting again: {sequence:?}"
        );
        assert!(
            before.0 > 0,
            "and it had stood somewhere: a pane that had said nothing would \
             make the comparison above vacuous"
        );
        // And the pane is the same pane, by the name it has anywhere.
        let named = GlobalPaneId {
            host: HostId(host.clone()),
            pane: PANE,
        };
        assert_eq!(
            GlobalPaneId::parse(&named.to_string()),
            Ok(named),
            "under the address it had before the drop"
        );
        drop(manager);
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// The byte a host's first subscription has reached.
///
/// # Errors
///
/// When it holds none.
fn cursor(held: &HostManager, host: &str) -> Result<iznik_protocol::identity::Sequence, Failed> {
    held.model()
        .host(&HostId(host.to_owned()))
        .and_then(|view| view.subscriptions.values().next().map(|found| found.cursor))
        .ok_or_else(|| "the host holds no subscription".into())
}

/// # Panics
///
/// When an optimistic command does not show at once, or a refused one is not
/// put back.
#[test]
fn connection_manager_shows_a_rename_before_the_host_agrees() {
    let case = || -> Result<(), Failed> {
        let held = scratch("optimistic")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(stack.socket());
        manager.add_host(&host);
        await_connected(&events, &[&host])?;
        let session = make_a_session(&manager, &events, &host, "before")?;
        let submission = manager.command(
            &host,
            SessionCommand::RenameSession {
                session,
                name: "after".to_owned(),
            },
        )?;
        assert!(submission.optimistic, "a rename shows at once");
        assert_eq!(
            first_name(&manager, &host),
            Some("after".to_owned()),
            "before the host has said anything"
        );
        let _answered = await_event(&events, "the rename's answer", PROMPT, |event| {
            matches!(
                event,
                ManagerEvent::Notify(Notification::CommandFinished { command, .. })
                    if *command == submission.id
            )
        })?;
        assert_eq!(
            first_name(&manager, &host),
            Some("after".to_owned()),
            "and stays after it agrees"
        );
        // One the host refuses is put back.
        let refused = manager.command(
            &host,
            SessionCommand::RenameSession {
                session: SessionId(u64::MAX),
                name: "nowhere".to_owned(),
            },
        )?;
        let answered = await_event(&events, "the refusal", PROMPT, |event| {
            matches!(
                event,
                ManagerEvent::Notify(Notification::CommandFinished { command, .. })
                    if *command == refused.id
            )
        })?;
        let ManagerEvent::Notify(Notification::CommandFinished { outcome, .. }) = answered else {
            return Err("the answer is a command result".into());
        };
        assert!(
            matches!(
                outcome,
                CommandOutcome::Rejected {
                    code: RejectionCode::UnknownSession,
                    ..
                }
            ),
            "the host refuses a session it does not hold: {outcome:?}"
        );
        assert_eq!(
            first_name(&manager, &host),
            Some("after".to_owned()),
            "and what was there is still there"
        );
        drop(manager);
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// The name of a host's first session, as the model holds it.
fn first_name(held: &HostManager, host: &str) -> Option<String> {
    held.model()
        .host(&HostId(host.to_owned()))?
        .model
        .sessions
        .first()
        .map(|session| session.name.clone())
}

/// # Panics
///
/// When a command a host never answers is left showing for ever.
#[test]
fn connection_manager_gives_up_on_a_host_that_never_answers() {
    let case = || -> Result<(), Failed> {
        let held = scratch("timeout")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let carried = relay(&runtime, &held, stack.socket())?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(&carried.socket);
        manager.add_host(&host);
        await_connected(&events, &[&host])?;
        let session = make_a_session(&manager, &events, &host, "before")?;
        // The link stops carrying, and a command goes into it.
        carried.cut();
        let submission = manager.command(
            &host,
            SessionCommand::RenameSession {
                session,
                name: "after".to_owned(),
            },
        )?;
        let timed = await_event(&events, "the command being given up on", PROMPT, |event| {
            matches!(
                event,
                ManagerEvent::Notify(Notification::CommandTimedOut { command, .. })
                    if *command == submission.id
            )
        })?;
        assert!(
            matches!(
                timed,
                ManagerEvent::Notify(Notification::CommandTimedOut { .. })
            ),
            "a command nobody answered is given up on by name"
        );
        assert_eq!(
            first_name(&manager, &host),
            Some("before".to_owned()),
            "and what it showed is put back, because a screen in a state the \
             host never agreed to is worse than a visible failure"
        );
        drop(manager);
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When four hosts that failed together schedule their retries together.
#[test]
fn connection_manager_does_not_let_four_hosts_retry_at_once() {
    let case = || -> Result<(), Failed> {
        let held = scratch("storm")?;
        // Observe scheduled waits without waiting them out. The wide jitter
        // band lets measurement intervals prove separation despite scheduling.
        let manager =
            manager_with_backoff(&held, Duration::from_hours(1), Duration::from_hours(1))?;
        let events = manager.events();
        let started = Instant::now();
        for index in 0..4_usize {
            manager.add_host(&alias(&held.path.join(format!("gone-{index}.sock"))));
        }
        let mut moments = std::collections::BTreeMap::new();
        while moments.len() < 4 {
            let failed = await_event(&events, "a host failing", PROMPT, |event| {
                matches!(
                    event,
                    ManagerEvent::Moved {
                        state: HostState::Failed { .. },
                        ..
                    }
                )
            })?;
            if let ManagerEvent::Moved {
                host,
                state: HostState::Failed { retry_at, .. },
            } = failed
            {
                // Failure occurred between submission and observation. The
                // original scheduled wait lies inside this entire interval.
                let minimum = retry_at.saturating_duration_since(Instant::now());
                let maximum = retry_at.saturating_duration_since(started);
                let _first = moments.entry(host).or_insert((minimum, maximum));
            }
        }
        let mut apart: Vec<_> = moments.values().copied().collect();
        apart.sort_unstable();
        for pair in apart.windows(2) {
            let [(.., maximum), (minimum, ..)] = pair else {
                panic!("two observation intervals");
            };
            assert!(
                minimum.saturating_sub(*maximum) > JITTER_FLOOR,
                "original waits are separated even after observation uncertainty: {moments:?}"
            );
        }
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the states a host went through are not reported in the order it went
/// through them.
#[test]
fn connection_manager_reports_every_state_in_order() {
    let case = || -> Result<(), Failed> {
        let held = scratch("events")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(stack.socket());
        manager.add_host(&host);
        let mut seen = Vec::new();
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        while Instant::now() < expires {
            let Ok(event) = events.recv_timeout(BRIEF) else {
                break;
            };
            if let ManagerEvent::Moved { state, .. } = event {
                let connected = matches!(state, HostState::Connected { .. });
                seen.push(state.to_string());
                if connected {
                    break;
                }
            }
        }
        assert_eq!(
            seen.first().map(String::as_str),
            Some("probing"),
            "it says it is probing first: {seen:?}"
        );
        assert!(
            seen.last()
                .is_some_and(|last| last.starts_with("connected")),
            "and connected last: {seen:?}"
        );
        assert!(
            !manager.model().is_empty(),
            "and the model holds what it heard"
        );
        drop(manager);
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
