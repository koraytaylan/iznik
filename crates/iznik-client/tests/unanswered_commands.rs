//! A command whose link goes before its answer arrives.
//!
//! The host may have applied it the moment before the link went, so it is not
//! a command that timed out and it is not one that failed: its outcome is
//! unknown, and the snapshot the next connection begins with says which. What
//! this client must not do is tell its caller the command was never answered
//! — a caller told that sends a creation again, and gets two.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::BackoffPolicy;
use iznik_client::reduce::Notification;
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
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

/// Serves one connection: a greeting and a model, and — the first time — a
/// link that goes the moment a command arrives, with no answer.
async fn one_connection(stream: tokio::net::UnixStream, first: bool) {
    let mut link = FramedLink::new(stream);
    loop {
        let asked = {
            let Ok(Some(frame)) = link.next_frame().await else {
                return;
            };
            decode_to_server(frame.payload)
        };
        let said = match asked {
            Ok(ToServer::Hello { .. }) => ToClient::Hello {
                protocol_version: PROTOCOL_VERSION,
                server_version: "scripted".to_owned(),
                capabilities: Capabilities::from_bits(0),
                instance: None,
                build: None,
            },
            Ok(ToServer::SnapshotRequest) => {
                let model = one_session();
                let Ok(payload) = encode_host_model(&model) else {
                    return;
                };
                ToClient::Snapshot {
                    generation: model.generation,
                    payload,
                }
            }
            // Applied or not, nobody here will ever say.
            Ok(ToServer::Command { .. }) if first => return,
            Ok(_otherwise) => continue,
            Err(_unreadable) => return,
        };
        let Ok(bytes) = encode_to_client(&said) else {
            return;
        };
        if link.send(CHANNEL_CONTROL, &bytes).await.is_err() {
            return;
        }
    }
}

/// # Panics
///
/// When a command whose link went is reported as timed out, or not reported.
#[test]
fn unanswered_commands_a_lost_link_makes_the_outcome_unknown() {
    let case = || -> Result<(), Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let held = Scratch {
            path: base.join(format!("iznik-unanswered-{}", std::process::id())),
        };
        let _gone = std::fs::remove_dir_all(&held.path);
        std::fs::create_dir_all(&held.path)?;
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        let socket = held.path.join("scripted.sock");
        let listener = runtime.block_on(async { UnixListener::bind(&socket) })?;
        let connections = Arc::new(AtomicUsize::new(0));
        let counting = Arc::clone(&connections);
        let _serving = runtime.spawn(async move {
            while let Ok((stream, _from)) = listener.accept().await {
                let first = counting.fetch_add(1, Ordering::AcqRel) == 0;
                one_connection(stream, first).await;
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
            pong_deadline: Duration::from_millis(400),
            open_deadline: Duration::from_secs(5),
            greeting_deadline: Duration::from_secs(1),
        };
        // Far past the case, so a timeout cannot be what reports it.
        options.pending_command_timeout = Duration::from_mins(1);
        options.expire_interval = Duration::from_millis(20);
        let manager = HostManager::new(options)?;
        let events = manager.events();
        let host = format!("{LOCAL_PREFIX}{}", socket.display());
        manager.add_host(&host)?;
        let told: Mutex<Vec<Notification>> = Mutex::new(Vec::new());
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
                    let unknown =
                        matches!(notification, Notification::CommandOutcomeUnknown { .. });
                    told.lock()
                        .map_err(|_broken| "poisoned")?
                        .push(notification);
                    if unknown {
                        break;
                    }
                }
                _otherwise => {}
            }
        }
        let command = sent.ok_or("the host's model never arrived")?;
        let told = told.into_inner().map_err(|_broken| "poisoned")?;
        assert!(
            told.iter().any(|notification| matches!(
                notification,
                Notification::CommandOutcomeUnknown { command: said, .. } if *said == command
            )),
            "the command whose link went is of unknown outcome: {told:?}"
        );
        assert!(
            !told
                .iter()
                .any(|notification| matches!(notification, Notification::CommandTimedOut { .. })),
            "and it is not reported as never answered: {told:?}"
        );
        let shown = manager
            .model()
            .host(&iznik_client::host::identity::HostId(host.clone()))
            .and_then(|view| {
                view.model
                    .sessions
                    .first()
                    .map(|session| session.name.clone())
            });
        assert_eq!(shown.as_deref(), Some("main"), "what it showed is put back");
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
