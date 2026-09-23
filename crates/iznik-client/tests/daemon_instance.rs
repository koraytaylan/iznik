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
use iznik_client::host::state::BackoffPolicy;
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
/// the first time — or never, the second.
async fn one_connection(
    stream: tokio::net::UnixStream,
    number: usize,
    instance: DaemonInstance,
    heard: &Heard,
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
                }]
            }
            _otherwise => Vec::new(),
        };
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
            // Gone, as a daemon that is restarted goes.
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
) -> Result<(), Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let keeping = Arc::clone(heard);
    let _serving = runtime.spawn(async move {
        let mut number = 0_usize;
        while let Ok((stream, _from)) = listener.accept().await {
            let instance = if number == 0 { FIRST } else { later };
            one_connection(stream, number, instance, &keeping).await;
            number = number.saturating_add(1);
        }
    });
    Ok(())
}

/// A manager that reconnects in milliseconds.
///
/// # Errors
///
/// When it cannot be built.
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
        greeting_deadline: Duration::from_secs(1),
    };
    Ok(HostManager::new(options)?)
}

/// Subscribes to the pane on a host whose daemon is replaced after the first
/// connection by `later`, and gives back what the second connection was asked
/// about the pane.
///
/// # Errors
///
/// When the host cannot be stood up or the second connection never asks.
fn asked_again(case: &str, later: DaemonInstance) -> Result<ToServer, Failed> {
    let held = scratch(case)?;
    let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
    let socket = held.path.join("scripted.sock");
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    scripted(&runtime, &socket, later, &heard)?;
    let manager = manager(&held)?;
    let events = manager.events();
    let host = format!("{LOCAL_PREFIX}{}", socket.display());
    manager.add_host(&host)?;
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut subscribed = false;
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if !subscribed && matches!(event, ManagerEvent::Snapshot { .. }) {
            manager.subscribe(&host, PANE)?;
            subscribed = true;
        }
        let again = heard.lock().ok().and_then(|recorded| {
            recorded
                .iter()
                .find(|(number, asked)| {
                    *number > 0
                        && matches!(
                            asked,
                            ToServer::Subscribe { pane } | ToServer::Resume { pane, .. }
                                if *pane == PANE
                        )
                })
                .map(|(_number, asked)| asked.clone())
        });
        if let Some(asked) = again {
            drop(manager);
            return Ok(asked);
        }
    }
    Err("the second connection was never asked for the pane".into())
}

/// # Panics
///
/// When a pane of a daemon that is gone is resumed from the byte it reached.
#[test]
fn daemon_instance_a_replaced_daemon_is_asked_for_every_pane_afresh() {
    let asked = asked_again("replaced", SECOND).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        asked,
        ToServer::Subscribe { pane: PANE },
        "a pane number of another daemon is subscribed afresh, never resumed"
    );
}

/// # Panics
///
/// When the same daemon is not resumed from the byte this client holds.
#[test]
fn daemon_instance_the_same_daemon_is_resumed_where_it_left_off() {
    let asked = asked_again("same", FIRST).unwrap_or_else(|error| panic!("{error}"));
    let reached = u64::try_from(SAID.len()).unwrap_or(u64::MAX);
    assert_eq!(
        asked,
        ToServer::Resume {
            pane: PANE,
            from_sequence: Sequence(reached),
        },
        "the same daemon carries on from the byte this client reached"
    );
}
