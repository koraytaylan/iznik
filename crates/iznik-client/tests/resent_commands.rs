//! A command whose link goes before its answer arrives, to a host that
//! remembers what it answered: sent again on the next link, under the same
//! number and the same client identity, and answered once.
//!
//! The host here is scripted. The first time a command arrives it closes the
//! link without a word, as a host that applied it the moment before the link
//! went would look; the second time it answers. What the case holds the
//! client to is that it names itself the same way on both links, sends the
//! same command under the same number, and hears one outcome — never an
//! unknown one.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_client::bootstrap::launch::bundled;
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::BackoffPolicy;
use iznik_client::reduce::Notification;
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, Created, SessionCommand, encode_command_outcome};
use iznik_protocol::identity::{
    ClientIdentity, CommandId, DaemonInstance, Generation, PaneId, SessionId, TabId,
};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};
use tokio::net::UnixListener;
use tokio::runtime::Builder as RuntimeBuilder;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// The session the command renames.
const SESSION: SessionId = SessionId(1);

/// What the scripted host heard: on which connection, whom and what.
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

/// A model holding one session with one pane.
fn one_session() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SESSION,
            name: "main".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                panes: vec![Pane {
                    id: PaneId(1),
                    title: String::new(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(PaneId(1)),
            }],
        }],
    }
}

/// What the scripted host does about one request.
enum Script {
    /// Answers it.
    Answer(ToClient),
    /// Says nothing and carries on.
    Nothing,
    /// Closes the link without a word.
    Close,
}

/// What the scripted host does about `asked` on its first connection, or on
/// a later one.
fn said(asked: &ToServer, first: bool) -> Script {
    match asked {
        ToServer::Hello { .. } => Script::Answer(ToClient::Hello {
            protocol_version: PROTOCOL_VERSION,
            server_version: bundled().crate_version,
            capabilities: Capabilities::from_bits(
                Capabilities::known().bits() & !Capabilities::ZSTD.bits(),
            ),
            instance: Some(DaemonInstance(42)),
            build: None,
        }),
        ToServer::SnapshotRequest => {
            let model = one_session();
            encode_host_model(&model).map_or(Script::Close, |payload| {
                Script::Answer(ToClient::Snapshot {
                    generation: model.generation,
                    payload,
                })
            })
        }
        ToServer::Ping => Script::Answer(ToClient::Pong),
        // Applied or not, nobody here will say — the first time.
        ToServer::Command { .. } if first => Script::Close,
        ToServer::Command { command_id, .. } => {
            let outcome = CommandOutcome::Applied {
                generation: Generation(1),
                created: Created::Nothing,
            };
            encode_command_outcome(&outcome).map_or(Script::Close, |payload| {
                Script::Answer(ToClient::CommandResult {
                    command_id: *command_id,
                    payload,
                })
            })
        }
        _otherwise => Script::Nothing,
    }
}

/// Serves one connection as [`said`] scripts it, keeping what it heard.
async fn one_connection(stream: tokio::net::UnixStream, number: usize, heard: Heard) {
    let mut link = FramedLink::new(stream);
    loop {
        let Ok(Some(frame)) = link.next_frame().await else {
            return;
        };
        let Ok(asked) = decode_to_server(frame.payload) else {
            return;
        };
        let answer = said(&asked, number == 0);
        if let Ok(mut held) = heard.lock() {
            held.push((number, asked));
        }
        let answer = match answer {
            Script::Answer(answer) => answer,
            Script::Nothing => continue,
            Script::Close => return,
        };
        let Ok(bytes) = encode_to_client(&answer) else {
            return;
        };
        if link.send(CHANNEL_CONTROL, &bytes).await.is_err() {
            return;
        }
    }
}

/// Who named itself on connection `number`, and what commands it sent there.
fn on(heard: &[(usize, ToServer)], number: usize) -> (Vec<ClientIdentity>, Vec<CommandId>) {
    let mut named = Vec::new();
    let mut commands = Vec::new();
    for (connection, asked) in heard {
        match asked {
            ToServer::Identify { client } if *connection == number => named.push(*client),
            ToServer::Command { command_id, .. } if *connection == number => {
                commands.push(*command_id);
            }
            _otherwise => {}
        }
    }
    (named, commands)
}

/// Runs a manager on the scripted host at `socket`: renames the session once
/// the model arrives, and gathers what it is told until the rename is
/// finished one way or the other. Gives back the rename's number, when it was
/// sent, and everything told.
///
/// # Errors
///
/// When the manager will not start or the command will not go.
fn drive(
    options: ManagerOptions,
    socket: &std::path::Path,
) -> Result<(Option<CommandId>, Vec<Notification>), Failed> {
    let manager = HostManager::new(options)?;
    let events = manager.events();
    let host = format!("{LOCAL_PREFIX}{}", socket.display());
    manager.add_host(&host)?;
    let mut told = Vec::new();
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut sent = None;
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        match event {
            ManagerEvent::Snapshot { .. } if sent.is_none() => {
                let submission = manager.command(
                    &host,
                    SessionCommand::RenameSession {
                        session: SESSION,
                        name: "renamed".to_owned(),
                    },
                )?;
                sent = Some(submission.id);
            }
            ManagerEvent::Notify(notification) => {
                let finished = matches!(
                    notification,
                    Notification::CommandFinished { .. }
                        | Notification::CommandOutcomeUnknown { .. }
                );
                told.push(notification);
                if finished {
                    break;
                }
            }
            _otherwise => {}
        }
    }
    Ok((sent, told))
}

/// # Panics
///
/// When a command whose link went is not sent again on the next link under
/// the same number and identity, or is reported as of unknown outcome, or
/// its answer on the next link is not heard.
#[test]
fn resent_commands_a_lost_link_is_answered_on_the_next() {
    let case = || -> Result<(), Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let held = Scratch {
            path: base.join(format!("iznik-resent-{}", std::process::id())),
        };
        let _gone = std::fs::remove_dir_all(&held.path);
        std::fs::create_dir_all(&held.path)?;
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        let socket = held.path.join("scripted.sock");
        let listener = runtime.block_on(async { UnixListener::bind(&socket) })?;
        let heard: Heard = Arc::new(Mutex::new(Vec::new()));
        let hearing = Arc::clone(&heard);
        let connections = Arc::new(AtomicUsize::new(0));
        let _serving = runtime.spawn(async move {
            while let Ok((stream, _from)) = listener.accept().await {
                let number = connections.fetch_add(1, Ordering::AcqRel);
                tokio::spawn(one_connection(stream, number, Arc::clone(&hearing)));
            }
        });
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
            pong_deadline: Duration::from_secs(2),
            open_deadline: Duration::from_secs(5),
            greeting_deadline: Duration::from_secs(1),
        };
        // Far past the case, so a timeout cannot be what settles it.
        options.pending_command_timeout = Duration::from_mins(1);
        options.expire_interval = Duration::from_millis(20);
        let (sent, told) = drive(options, &socket)?;
        let command = sent.ok_or("the host's model never arrived")?;
        assert!(
            told.iter().any(|notification| matches!(
                notification,
                Notification::CommandFinished { command: said, outcome: CommandOutcome::Applied { .. }, .. }
                    if *said == command
            )),
            "the command sent again is answered: {told:?}"
        );
        assert!(
            !told.iter().any(|notification| matches!(
                notification,
                Notification::CommandOutcomeUnknown { .. }
            )),
            "and never of unknown outcome: {told:?}"
        );
        let heard = heard.lock().map_err(|_broken| "poisoned")?.clone();
        let (first_named, first_sent) = on(&heard, 0);
        let (again_named, again_sent) = on(&heard, 1);
        assert_eq!(first_sent, vec![command], "the first link carried it");
        assert_eq!(
            again_sent,
            vec![command],
            "the next, once, under its number"
        );
        assert_eq!(first_named.len(), 1, "the client named itself: {heard:?}");
        assert_eq!(again_named, first_named, "and the same way both times");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
