//! A host whose daemon was replaced between two connections is a fresh host.
//!
//! Pane numbers begin again with every daemon, so a cursor this client holds
//! for pane 1 is a position in a pane that is gone. Resuming it from a daemon
//! whose own pane 1 happens to be long enough would splice another pane's
//! bytes onto the screen; what the daemon instance in `Hello` is for is that a
//! client can tell, and ask for every pane afresh instead.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::{BackoffPolicy, HostState};
use iznik_client::reduce::Notification;
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{DaemonInstance, Generation, PaneId, Sequence, SessionId, TabId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};
use tokio::net::UnixListener;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long a host waits to be tried again when nothing is asked meanwhile.
const QUICK: Duration = Duration::from_millis(20);

/// How long it waits when something is asked while its link is down: long
/// enough that the asking lands before the next connection.
const HELD_DOWN: Duration = Duration::from_millis(500);

/// How long a case lets a connection go on being asked after its first ask
/// about the pane.
const SETTLE: Duration = Duration::from_millis(300);

/// The pane both daemons hold under the same number.
const PANE: PaneId = PaneId(1);

/// The channel the scripted host carries it on.
const CHANNEL: u8 = 1;

/// What the first daemon's pane says before its link goes.
const SAID: &[u8] = b"hello";

/// The first daemon.
const FIRST: DaemonInstance = DaemonInstance(0x1111);

/// Another one.
const SECOND: DaemonInstance = DaemonInstance(0x2222);

/// What a connection was asked, with the number of the connection it came on.
type Heard = Arc<Mutex<Vec<(usize, ToServer)>>>;

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
    let path = base.join(format!("iznik-instance-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A model holding one pane, numbered as both daemons number it.
fn one_pane() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "main".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                panes: vec![Pane {
                    id: PANE,
                    title: String::new(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(PANE),
            }],
        }],
    }
}

/// Serves one connection as the daemon `instance`, recording what it is asked
/// under `number`, and closes it once a subscribed pane has said [`SAID`] —
/// the first time — or never, the second. When `hide`, the first connection
/// closes only once the pane has been let go of, which it answers as a host
/// does.
async fn one_connection(
    stream: tokio::net::UnixStream,
    number: usize,
    instance: DaemonInstance,
    heard: &Heard,
    hide: bool,
) {
    let mut link = FramedLink::new(stream);
    loop {
        let asked = {
            let Ok(Some(frame)) = link.next_frame().await else {
                return;
            };
            decode_to_server(frame.payload)
        };
        let Ok(asked) = asked else { return };
        if let Ok(mut held) = heard.lock() {
            held.push((number, asked.clone()));
        }
        let said = match asked {
            ToServer::Hello { .. } => vec![ToClient::Hello {
                protocol_version: PROTOCOL_VERSION,
                server_version: "scripted".to_owned(),
                capabilities: Capabilities::INSTANCE,
                instance: Some(instance),
            }],
            ToServer::SnapshotRequest => {
                let model = one_pane();
                let Ok(payload) = encode_host_model(&model) else {
                    return;
                };
                vec![ToClient::Snapshot {
                    generation: model.generation,
                    payload,
                }]
            }
            ToServer::Subscribe { pane } | ToServer::Resume { pane, .. } => {
                vec![ToClient::PaneChannel {
                    pane,
                    channel: CHANNEL,
                    sequence: Sequence(0),
                    answered_through: None,
                }]
            }
            ToServer::Unsubscribe { pane } => vec![ToClient::PaneDetached {
                pane,
                channel: CHANNEL,
            }],
            _otherwise => Vec::new(),
        };
        let detached = said
            .iter()
            .any(|message| matches!(message, ToClient::PaneDetached { .. }));
        let subscribed = said
            .iter()
            .any(|message| matches!(message, ToClient::PaneChannel { .. }));
        for message in said {
            let Ok(bytes) = encode_to_client(&message) else {
                return;
            };
            if link.send(CHANNEL_CONTROL, &bytes).await.is_err() {
                return;
            }
        }
        if subscribed && number == 0 {
            let _said = link.send(CHANNEL, SAID).await;
        }
        // Gone, as a daemon that is restarted goes.
        if number == 0 && ((subscribed && !hide) || detached) {
            return;
        }
    }
}

/// A host whose first connection is daemon [`FIRST`] and whose every later one
/// is `later`.
///
/// # Errors
///
/// When the socket cannot be bound.
fn scripted(
    runtime: &Runtime,
    socket: &std::path::Path,
    later: DaemonInstance,
    heard: &Heard,
    hide: bool,
) -> Result<(), Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let keeping = Arc::clone(heard);
    let _serving = runtime.spawn(async move {
        let mut number = 0_usize;
        while let Ok((stream, _from)) = listener.accept().await {
            let instance = if number == 0 { FIRST } else { later };
            one_connection(stream, number, instance, &keeping, hide).await;
            number = number.saturating_add(1);
        }
    });
    Ok(())
}

/// A manager that reconnects after `backoff`.
///
/// # Errors
///
/// When it cannot be built.
fn manager(held: &Scratch, backoff: Duration) -> Result<HostManager, Failed> {
    let artifacts = held.path.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let mut options = ManagerOptions::new(artifacts, paths);
    options.backoff = BackoffPolicy {
        initial: backoff,
        maximum: backoff,
        ..BackoffPolicy::default()
    };
    options.channel = ChannelOptions {
        ping_interval: Duration::from_millis(50),
        pong_deadline: Duration::from_millis(400),
        open_deadline: Duration::from_secs(5),
        greeting_deadline: Duration::from_secs(1),
    };
    Ok(HostManager::new(options)?)
}

/// What an application does in a case, besides subscribing to the pane.
#[derive(Clone, Copy)]
enum Doing {
    /// Nothing.
    Nothing,
    /// Asks for the pane to be resumed from this byte while the host has no
    /// link — as an application showing a hidden pane again does.
    ResumeWhileDown(Sequence),
    /// Lets the pane go once its bytes arrive, and once the host is reached
    /// again asks for it to be resumed from this byte: a pane hidden before
    /// the daemon restarted, shown again after.
    HideThenResume(Sequence),
}

/// What a case saw.
struct Seen {
    /// Everything the later connections were asked about the pane.
    asked: Vec<ToServer>,
    /// Whether the application was told the daemon had been restarted.
    told: bool,
}

/// Whether a message asks for the pane.
fn about_the_pane(asked: &ToServer) -> bool {
    matches!(
        asked,
        ToServer::Subscribe { pane } | ToServer::Resume { pane, .. } if *pane == PANE
    )
}

/// What every connection after the first asked about the pane.
fn asked_later(heard: &Heard) -> Vec<ToServer> {
    heard
        .lock()
        .map(|recorded| {
            recorded
                .iter()
                .filter(|(number, asked)| *number > 0 && about_the_pane(asked))
                .map(|(_number, asked)| asked.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Does what `doing` asks at `event`, having seen `connected` connections.
///
/// # Errors
///
/// When the manager refuses.
fn act(
    manager: &HostManager,
    host: &str,
    event: &ManagerEvent,
    doing: Doing,
    connected: &mut usize,
) -> Result<(), Failed> {
    let ManagerEvent::Moved { state, .. } = event else {
        if let (ManagerEvent::Bytes { pane, .. }, Doing::HideThenResume(_)) = (event, doing)
            && *pane == PANE
            && *connected == 1
        {
            manager.unsubscribe(host, PANE)?;
        }
        return Ok(());
    };
    match (state, doing) {
        (HostState::Connected { .. }, _) => {
            *connected = connected.saturating_add(1);
            if let (Doing::HideThenResume(from), 2) = (doing, *connected) {
                manager.resume(host, PANE, from)?;
            }
        }
        (HostState::Reconnecting { .. }, Doing::ResumeWhileDown(from)) if *connected == 1 => {
            manager.resume(host, PANE, from)?;
        }
        _otherwise => {}
    }
    Ok(())
}

/// Subscribes to the pane on a host whose daemon is replaced after the first
/// connection by `later`, does what `doing` says, and gives back what it saw.
///
/// # Errors
///
/// When the host cannot be stood up or the second connection never asks.
fn asked_again(case: &str, later: DaemonInstance, doing: Doing) -> Result<Seen, Failed> {
    let held = scratch(case)?;
    let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
    let socket = held.path.join("scripted.sock");
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    let hide = matches!(doing, Doing::HideThenResume(_));
    scripted(&runtime, &socket, later, &heard, hide)?;
    // Long enough, when something is to be asked while the link is down, that
    // it is asked before the next connection rather than after.
    let backoff = match doing {
        Doing::ResumeWhileDown(_) => HELD_DOWN,
        Doing::Nothing | Doing::HideThenResume(_) => QUICK,
    };
    let manager = manager(&held, backoff)?;
    let events = manager.events();
    let host = format!("{LOCAL_PREFIX}{}", socket.display());
    manager.add_host(&host)?;
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut subscribed = false;
    let mut connected = 0_usize;
    let mut told = false;
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if !subscribed && matches!(event, ManagerEvent::Snapshot { .. }) {
            manager.subscribe(&host, PANE)?;
            subscribed = true;
        }
        told |= matches!(
            event,
            ManagerEvent::Notify(Notification::DaemonRestarted { .. })
        );
        act(&manager, &host, &event, doing, &mut connected)?;
        if !asked_later(&heard).is_empty() {
            // Whatever else the connection is going to be asked about the
            // pane, it is asked in the same breath.
            std::thread::sleep(SETTLE);
            told |= events.try_iter().any(|pending| {
                matches!(
                    pending,
                    ManagerEvent::Notify(Notification::DaemonRestarted { .. })
                )
            });
            drop(manager);
            return Ok(Seen {
                asked: asked_later(&heard),
                told,
            });
        }
    }
    Err("the second connection was never asked for the pane".into())
}

/// # Panics
///
/// When a pane of a daemon that is gone is resumed from the byte it reached,
/// or the application is not told the daemon was restarted.
#[test]
fn daemon_instance_a_replaced_daemon_is_asked_for_every_pane_afresh() {
    let seen =
        asked_again("replaced", SECOND, Doing::Nothing).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        seen.asked.first(),
        Some(&ToServer::Subscribe { pane: PANE }),
        "a pane number of another daemon is subscribed afresh, never resumed"
    );
    assert!(
        seen.told,
        "and the application is told the daemon restarted"
    );
}

/// # Panics
///
/// When the same daemon is not resumed from the byte this client holds, or is
/// said to have restarted.
#[test]
fn daemon_instance_the_same_daemon_is_resumed_where_it_left_off() {
    let seen = asked_again("same", FIRST, Doing::Nothing).unwrap_or_else(|error| panic!("{error}"));
    let reached = u64::try_from(SAID.len()).unwrap_or(u64::MAX);
    assert_eq!(
        seen.asked.first(),
        Some(&ToServer::Resume {
            pane: PANE,
            from_sequence: Sequence(reached),
        }),
        "the same daemon carries on from the byte this client reached"
    );
    assert!(!seen.told, "and nothing says it restarted");
}

/// # Panics
///
/// When a resume asked for while the link was down is carried, as a resume,
/// to a daemon other than the one its byte came from.
#[test]
fn daemon_instance_a_resume_held_through_a_restart_is_asked_afresh() {
    let reached = u64::try_from(SAID.len()).unwrap_or(u64::MAX);
    let seen = asked_again("held", SECOND, Doing::ResumeWhileDown(Sequence(reached)))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        seen.asked
            .iter()
            .all(|message| matches!(message, ToServer::Subscribe { .. })),
        "a held resume is asked afresh of a daemon that is another run: {:?}",
        seen.asked
    );
}

/// # Panics
///
/// When a pane hidden before the daemon restarted, and shown again after, is
/// resumed from a byte of the daemon that is gone.
#[test]
fn daemon_instance_a_pane_hidden_through_a_restart_is_asked_afresh() {
    let reached = u64::try_from(SAID.len()).unwrap_or(u64::MAX);
    let seen = asked_again("hidden", SECOND, Doing::HideThenResume(Sequence(reached)))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        seen.asked,
        vec![ToServer::Subscribe { pane: PANE }],
        "the pane is asked for afresh, never resumed from the old daemon's byte"
    );
}

/// # Panics
///
/// When a pane hidden and shown again on the same daemon is not resumed from
/// the byte the application holds.
#[test]
fn daemon_instance_a_pane_hidden_on_the_same_daemon_is_resumed() {
    let reached = u64::try_from(SAID.len()).unwrap_or(u64::MAX);
    let seen = asked_again("unhidden", FIRST, Doing::HideThenResume(Sequence(reached)))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        seen.asked,
        vec![ToServer::Resume {
            pane: PANE,
            from_sequence: Sequence(reached),
        }],
        "the same daemon carries on from the application's byte"
    );
}
