//! Keystrokes given while a host is still being reached are dropped, and the
//! application is told — never delivered late.
//!
//! A bootstrap can take minutes. A key pressed at its start and typed into the
//! pane when it ends lands in whatever the person has since moved on to.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::reduce::Notification;
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{Generation, PaneId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, encode_host_model};
use tokio::net::UnixListener;
use tokio::runtime::Builder as RuntimeBuilder;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// How long the host takes to say hello: the bootstrap the keystroke waits
/// behind.
const SLOW_GREETING: Duration = Duration::from_millis(300);

/// How long the case waits for the application to be told.
const PROMPT: Duration = Duration::from_secs(10);

/// How long it then gives a late keystroke to arrive at the host.
const SETTLE: Duration = Duration::from_millis(300);

/// The pane the keystrokes are for.
const PANE: PaneId = PaneId(1);

/// What is typed.
const TYPED: &[u8] = b"ls\n";

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

/// A host that takes [`SLOW_GREETING`] to say hello — the bootstrap a
/// keystroke waits behind — and counts every keystroke it is sent.
async fn slow_host(listener: UnixListener, counting: Arc<AtomicUsize>) {
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
            Ok(ToServer::Hello { .. }) => {
                tokio::time::sleep(SLOW_GREETING).await;
                encode_to_client(&ToClient::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    server_version: "scripted".to_owned(),
                    capabilities: Capabilities::from_bits(0),
                    instance: None,
                })
            }
            Ok(ToServer::SnapshotRequest) => {
                let model = HostModel {
                    generation: Generation(1),
                    sessions: Vec::new(),
                };
                let Ok(payload) = encode_host_model(&model) else {
                    return;
                };
                encode_to_client(&ToClient::Snapshot {
                    generation: model.generation,
                    payload,
                })
            }
            Ok(ToServer::Input { .. }) => {
                let _counted = counting.fetch_add(1, Ordering::AcqRel);
                continue;
            }
            Ok(_otherwise) => continue,
            Err(_unreadable) => return,
        };
        let Ok(said) = said else { return };
        if link.send(CHANNEL_CONTROL, &said).await.is_err() {
            return;
        }
    }
}

/// # Panics
///
/// When a keystroke given before the link was up reaches the host, or the
/// application is not told it was dropped.
#[test]
fn late_input_given_before_the_link_is_dropped_and_said() {
    let case = || -> Result<(), Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let held = Scratch {
            path: base.join(format!("iznik-late-input-{}", std::process::id())),
        };
        let _gone = std::fs::remove_dir_all(&held.path);
        std::fs::create_dir_all(&held.path)?;
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        let socket = held.path.join("scripted.sock");
        let listener = runtime.block_on(async { UnixListener::bind(&socket) })?;
        let typed = Arc::new(AtomicUsize::new(0));
        let counting = Arc::clone(&typed);
        let _serving = runtime.spawn(slow_host(listener, counting));
        let artifacts = held.path.join("artifacts");
        std::fs::create_dir_all(&artifacts)?;
        let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
        let mut options = ManagerOptions::new(artifacts, paths);
        options.channel = ChannelOptions {
            ping_interval: Duration::from_millis(50),
            pong_deadline: Duration::from_secs(5),
            open_deadline: Duration::from_secs(5),
            greeting_deadline: Duration::from_secs(5),
        };
        let manager = HostManager::new(options)?;
        let events = manager.events();
        let host = format!("{LOCAL_PREFIX}{}", socket.display());
        manager.add_host(&host)?;
        // Given while the host is still being reached.
        manager.input(&host, PANE, TYPED.to_vec())?;
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        let mut told = None;
        while told.is_none() && Instant::now() < expires {
            let left = expires.saturating_duration_since(Instant::now());
            let Ok(event) = events.recv_timeout(left) else {
                break;
            };
            if let ManagerEvent::Notify(Notification::InputDropped { pane, bytes, .. }) = event {
                told = Some((pane, bytes));
            }
        }
        assert_eq!(
            told,
            Some((PANE, TYPED.len())),
            "the application is told what was dropped"
        );
        std::thread::sleep(SETTLE);
        assert_eq!(
            typed.load(Ordering::Acquire),
            0,
            "and the host never receives it late"
        );
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
