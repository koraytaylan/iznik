//! What goes out to a host, and what comes back from it.
//!
//! Two things the loop between this client and one host has to get right, and
//! neither of them is visible from a value: that what an application hands
//! over goes out as fast as it was given rather than one thing to a round
//! trip, and that what comes back is passed on only when this client could
//! take it. Both stand a whole host up on this machine — a real daemon for
//! the first, a scripted one for the second, because no real host sends a
//! number it never reached.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::{BackoffPolicy, HostState};
use iznik_client::model::ClientModel;
use iznik_client::reduce::Notification;
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, Created, SessionCommand, encode_command_outcome};
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{Generation, PaneId, SessionId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, encode_host_model};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::net::UnixListener;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// The width a pane is made at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// The pane every one of these makes.
const PANE: PaneId = PaneId(1);

/// The backoff these cases run under: tens of milliseconds, so a case that
/// reconnects takes no longer than one that does not.
const QUICK_INITIAL: Duration = Duration::from_millis(20);

/// And a ceiling under a second.
const QUICK_MAXIMUM: Duration = Duration::from_millis(200);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The generation the scripted host's snapshot is.
const SETTLED: u64 = 1;

/// And the one its change claims to produce, which is not the one after it.
const SKIPPED: u64 = 99;

/// The generation a scripted host says a command reached.
const ANSWERED: u64 = 6;

/// And the one it comes back with after it has been replaced: a daemon that
/// was upgraded, or restarted, begins again at nothing.
const AGAIN: u64 = 0;

/// How many times the scripted host has been asked for its model when the
/// asking that follows a gap has happened.
const TWICE_ASKED: usize = 1;

/// How many times a line comes back from the reader: once as the terminal
/// echoed it, once as the reader wrote it back.
const TWICE: usize = 2;

/// How many round trips a hundred lines handed over at once may cost.
///
/// One, when they are carried as they are given; a hundred, when each waits
/// on the one before it. Ten is far from both, so which of the two happened
/// is legible however loaded the machine is — and a machine's load lengthens
/// the round trip this is counted in, not the count.
const ROUND_TRIPS: u32 = 10;

/// The least a hundred lines are given, however quick one line was.
///
/// A round trip on an idle machine is a fraction of a millisecond, and ten of
/// those is not a budget but a stopwatch on the scheduler.
const BURST_FLOOR: Duration = Duration::from_millis(250);

/// What says the reader is reading, for the same reason as [`ALONE`].
const READY: &[u8] = b"ready";

/// The line whose round trip the burst is measured against.
///
/// Letters, so that it is nobody's: the callers' lines are digits, and a
/// marker of their shape would be one of theirs — its arrival, from this
/// phase, would stand in for a line that was never carried.
const ALONE: &str = "alone";

/// How many lines are handed over at once in the burst case.
///
/// Enough that carrying them one to a turn would show: with a round trip
/// between each, a hundred of them take longer than any budget here.
const BURST: usize = 100;

/// What the burst is typed into, and what says it is ready.
///
/// A shell of its own re-arms its terminal before every line it reads, and
/// what it uses to do that throws away input that arrived but has not been
/// read — so a shell would make this a case about how far it had got. `cat`
/// re-arms nothing, and a line comes back from it twice: once as the terminal
/// echoed it, once as `cat` wrote it back.
const READER: &[u8] = b"cat\n";

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
    let artifacts = held.path.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let mut options = ManagerOptions::new(artifacts, paths);
    options.backoff = BackoffPolicy {
        initial: QUICK_INITIAL,
        maximum: QUICK_MAXIMUM,
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

/// Whether a needle is somewhere in a haystack.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len().max(1))
        .any(|window| window == needle)
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

/// Waits until everything a pane has said satisfies `wanted`, and gives back
/// what it said.
///
/// Bytes arrive in whatever pieces the host sent them, so a case that asks
/// about many lines has to gather rather than wait for one event.
///
/// # Errors
///
/// When it never does.
fn gathered(
    events: &Receiver<ManagerEvent>,
    what: &str,
    patience: Duration,
    wanted: impl Fn(&[u8]) -> bool,
) -> Result<Vec<u8>, Failed> {
    let expires = Instant::now().checked_add(patience).ok_or("no clock")?;
    let mut said = Vec::new();
    while Instant::now() < expires {
        if wanted(&said) {
            return Ok(said);
        }
        let left = expires.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(ManagerEvent::Bytes { pane, bytes, .. }) if pane == PANE => {
                said.extend_from_slice(&bytes);
            }
            Ok(_other) => {}
            Err(_nothing) => break,
        }
    }
    if wanted(&said) {
        return Ok(said);
    }
    Err(format!(
        "no {what} inside {patience:?}; the pane said {:?}",
        String::from_utf8_lossy(&said)
    )
    .into())
}

/// # Panics
///
/// When a burst of input is paced by what the host is saying rather than
/// carried as fast as it was handed over.
#[test]
fn manager_traffic_carries_a_burst_as_fast_as_it_is_given() {
    let case = || -> Result<(), Failed> {
        let held = scratch("burst")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(stack.socket());
        manager.add_host(&host);
        await_connected(&events, &[&host])?;
        let _session = make_a_session(&manager, &events, &host, "work")?;
        manager.subscribe(&host, PANE)?;
        manager.input(&host, PANE, READER.to_vec())?;
        // Not that the name was echoed — that what it named is reading, which
        // is what a line coming back twice says.
        manager.input(&host, PANE, b"#ready\n".to_vec())?;
        let _ready = gathered(&events, "the reader reading", PROMPT, |said| {
            said.windows(READY.len())
                .filter(|window| *window == READY)
                .count()
                >= TWICE
        })?;
        // What one line costs, so that what a hundred cost can be read
        // against it rather than against a clock a busy machine cannot keep.
        let alone = Instant::now();
        manager.input(&host, PANE, format!("#{ALONE}\n").into_bytes())?;
        let _once = gathered(&events, "one line coming back", PROMPT, |said| {
            said.windows(ALONE.len())
                .filter(|window| *window == ALONE.as_bytes())
                .count()
                >= TWICE
        })?;
        let one = alone.elapsed();
        // A hundred lines handed over at once, while the host is saying
        // something: every one of them is carried without waiting to hear
        // what it has to say between one and the next.
        let started = Instant::now();
        for index in 0..BURST {
            manager.input(&host, PANE, format!("#{index:02}{index:02}\n").into_bytes())?;
        }
        let _said = gathered(&events, "the burst", PROMPT, |said| {
            (0..BURST).all(|index| contains(said, format!("{index:02}{index:02}").as_bytes()))
        })?;
        let taken = started.elapsed();
        let budget = one.saturating_mul(ROUND_TRIPS).max(BURST_FLOOR);
        assert!(
            taken < budget,
            "a hundred lines cost {taken:?}, which is more than {ROUND_TRIPS} of the \
             {one:?} one line costs: they are being carried one to a round trip"
        );
        drop(manager);
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// A host that shakes hands, answers for its model, and then sends a change
/// numbered past the one that would follow.
///
/// Counts what it was asked for, so a case can tell that the client asked
/// again rather than guessing what the gap did to it.
///
/// # Errors
///
/// When the socket cannot be bound.
fn skips_a_number(
    runtime: &Runtime,
    socket: &std::path::Path,
    asked: &Arc<AtomicUsize>,
) -> Result<(), Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let counted = Arc::clone(asked);
    let _serving = runtime.spawn(async move {
        let Ok((stream, _from)) = listener.accept().await else {
            return;
        };
        let mut link = FramedLink::new(stream);
        loop {
            // The borrow the frame holds ends here, before anything is sent.
            let heard = {
                let Ok(Some(frame)) = link.next_frame().await else {
                    return;
                };
                decode_to_server(frame.payload)
            };
            match heard {
                Ok(ToServer::Hello { .. }) => {
                    let Ok(said) = encode_to_client(&ToClient::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: "scripted".to_owned(),
                        capabilities: Capabilities::from_bits(0),
                    }) else {
                        return;
                    };
                    let _sent = link.send(CHANNEL_CONTROL, &said).await;
                }
                Ok(ToServer::SnapshotRequest) => {
                    let first = counted.fetch_add(1, Ordering::AcqRel) == 0;
                    let held = HostModel {
                        generation: Generation(SETTLED),
                        sessions: Vec::new(),
                    };
                    let Ok(payload) = encode_host_model(&held) else {
                        return;
                    };
                    let Ok(said) = encode_to_client(&ToClient::Snapshot {
                        generation: Generation(SETTLED),
                        payload,
                    }) else {
                        return;
                    };
                    let _sent = link.send(CHANNEL_CONTROL, &said).await;
                    if !first {
                        continue;
                    }
                    // And then a change from a generation nothing here has
                    // reached: what a client sees when a message was lost.
                    let Ok(change) = encode_delta(&Delta::SessionRemoved {
                        session: SessionId(1),
                    }) else {
                        return;
                    };
                    let Ok(numbered) = encode_to_client(&ToClient::Delta {
                        generation: Generation(SKIPPED),
                        payload: change,
                    }) else {
                        return;
                    };
                    let _skipped = link.send(CHANNEL_CONTROL, &numbered).await;
                }
                Ok(_otherwise) => {}
                Err(_unreadable) => return,
            }
        }
    });
    Ok(())
}

/// # Panics
///
/// When a change this client could not take is passed on regardless, or when
/// nothing is done about the gap it left.
#[test]
fn manager_traffic_passes_on_no_change_it_could_not_take() {
    let case = || -> Result<(), Failed> {
        let held = scratch("gap")?;
        let runtime = runtime()?;
        let asked = Arc::new(AtomicUsize::new(0));
        let socket = held.path.join("scripted.sock");
        skips_a_number(&runtime, &socket, &asked)?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(&socket);
        manager.add_host(&host);
        // Everything it says, in the order it says it, and nothing thrown
        // away: a case that waited for one kind of event would discard the
        // very event it is about.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        let mut snapshots = 0_usize;
        while Instant::now() < expires && asked.load(Ordering::Acquire) <= TWICE_ASKED {
            let left = expires.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(ManagerEvent::Delta { generation, .. }) => {
                    panic!("a change this client could not take was passed on: {generation:?}")
                }
                Ok(ManagerEvent::Snapshot { .. }) => snapshots = snapshots.saturating_add(1),
                Ok(_otherwise) => {}
                Err(_nothing) => break,
            }
        }
        // And whatever is still queued behind what ended the loop.
        while let Ok(event) = events.try_recv() {
            if let ManagerEvent::Delta { generation, .. } = event {
                panic!("a change this client could not take was passed on: {generation:?}");
            }
        }
        assert!(snapshots > 0, "the host's model is passed on");
        assert!(
            asked.load(Ordering::Acquire) > TWICE_ASKED,
            "and the whole of it is asked for again, which is what a gap leaves to do"
        );
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// A host that answers a command and then comes back as a different daemon:
/// the same socket, a model numbered below the one it had.
///
/// # Errors
///
/// When the socket cannot be bound.
fn starts_again(runtime: &Runtime, socket: &std::path::Path) -> Result<(), Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let _serving = runtime.spawn(async move {
        let Ok((stream, _from)) = listener.accept().await else {
            return;
        };
        let mut link = FramedLink::new(stream);
        loop {
            let heard = {
                let Ok(Some(frame)) = link.next_frame().await else {
                    return;
                };
                decode_to_server(frame.payload)
            };
            let said = match heard {
                Ok(ToServer::Hello { .. }) => encode_to_client(&ToClient::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    server_version: "scripted".to_owned(),
                    capabilities: Capabilities::from_bits(0),
                }),
                Ok(ToServer::SnapshotRequest) => model_at(SETTLED),
                // Answered, and then never announced: the window this client
                // holds a command applied in, which is where a daemon that
                // goes away leaves one for ever if nothing notices.
                Ok(ToServer::Command { command_id, .. }) => {
                    let Ok(payload) = encode_command_outcome(&CommandOutcome::Applied {
                        generation: Generation(ANSWERED),
                        created: Created::Nothing,
                    }) else {
                        return;
                    };
                    let answer = encode_to_client(&ToClient::CommandResult {
                        command_id,
                        payload,
                    });
                    let Ok(answer) = answer else {
                        return;
                    };
                    let _answered = link.send(CHANNEL_CONTROL, &answer).await;
                    // And now it is another daemon, with another model.
                    model_at(AGAIN)
                }
                Ok(_otherwise) => continue,
                Err(_unreadable) => return,
            };
            let Ok(said) = said else {
                return;
            };
            let _sent = link.send(CHANNEL_CONTROL, &said).await;
        }
    });
    Ok(())
}

/// A snapshot of an empty model at one generation, encoded.
///
/// # Errors
///
/// When it cannot be encoded, which is what the caller stops on.
fn model_at(generation: u64) -> Result<Vec<u8>, iznik_protocol::message::MessageError> {
    let held = HostModel {
        generation: Generation(generation),
        sessions: Vec::new(),
    };
    let payload = encode_host_model(&held)?;
    encode_to_client(&ToClient::Snapshot {
        generation: Generation(generation),
        payload,
    })
}

/// # Panics
///
/// When a host that came back as another daemon is not passed on, or when
/// what the daemon that is gone answered goes on being shown.
#[test]
fn manager_traffic_passes_on_the_model_a_replaced_daemon_sends() {
    let case = || -> Result<(), Failed> {
        let held = scratch("again")?;
        let runtime = runtime()?;
        let socket = held.path.join("scripted.sock");
        starts_again(&runtime, &socket)?;
        let manager = manager(&held)?;
        let events = manager.events();
        let host = alias(&socket);
        manager.add_host(&host);
        await_connected(&events, &[&host])?;
        // A command the host answers and never announces, which is what this
        // client keeps applied until the model reaches the generation the
        // answer named — a generation the daemon that comes back never had.
        let submission = manager.command(
            &host,
            SessionCommand::RenameSession {
                session: SessionId(1),
                name: "renamed".to_owned(),
            },
        )?;
        assert!(submission.id.0 > 0, "the command was given a number");
        // The snapshot from the daemon that replaced it reaches the
        // application: giving up on a command is not a reason to keep the
        // host's own account of itself from whoever is watching.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        let mut replaced = false;
        while Instant::now() < expires && !replaced {
            let left = expires.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(ManagerEvent::Snapshot { generation, .. }) => {
                    replaced = generation == Generation(AGAIN);
                }
                Ok(_otherwise) => {}
                Err(_nothing) => break,
            }
        }
        assert!(
            replaced,
            "the model the daemon that replaced it sent is passed on"
        );
        // And nothing of the daemon that is gone is still in flight.
        let waiting = manager
            .model()
            .host(&HostId(host.clone()))
            .map_or(0, |view| view.pending.len());
        assert_eq!(waiting, 0, "and what it answered stops being shown");
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
