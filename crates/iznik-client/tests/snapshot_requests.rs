//! One snapshot asked for, however many changes arrive out of step before it
//! comes.
//!
//! Every delta after a gap is another gap until the whole model arrives, and a
//! client that asked again for each would have the host serialize its whole
//! model once per change — exactly while it is busiest.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use iznik_client::host::manager::{HostManager, ManagerOptions};
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{Generation, SessionId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, encode_host_model};
use tokio::net::UnixListener;
use tokio::runtime::Builder as RuntimeBuilder;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// How long the case gives the client to ask more than it should.
const SETTLE: Duration = Duration::from_millis(500);

/// How long it waits for the first asking at all.
const PROMPT: Duration = Duration::from_secs(10);

/// The generations the host's out-of-step changes claim, none of them the one
/// after the model's.
const SKIPPED: [u64; 3] = [99, 100, 101];

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

/// The messages a host answers a snapshot request with: an empty model, and —
/// the first time — three changes numbered past it, and never another
/// snapshot, so every one of them stays a gap.
///
/// # Errors
///
/// When one of them will not encode.
fn answer(first: bool) -> Result<Vec<ToClient>, iznik_protocol::message::MessageError> {
    let model = HostModel {
        generation: Generation(1),
        sessions: Vec::new(),
    };
    let mut said = vec![ToClient::Snapshot {
        generation: model.generation,
        payload: encode_host_model(&model)?,
    }];
    if first {
        for generation in SKIPPED {
            said.push(ToClient::Delta {
                generation: Generation(generation),
                payload: encode_delta(&Delta::SessionRemoved {
                    session: SessionId(1),
                })?,
            });
        }
    }
    Ok(said)
}

/// # Panics
///
/// When a client that fell out of step asks for the model more than once for
/// it.
#[test]
fn snapshot_requests_are_asked_once_for_a_run_of_gaps() {
    let case = || -> Result<(), Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let held = Scratch {
            path: base.join(format!("iznik-snapshots-{}", std::process::id())),
        };
        let _gone = std::fs::remove_dir_all(&held.path);
        std::fs::create_dir_all(&held.path)?;
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        let socket = held.path.join("scripted.sock");
        let listener = runtime.block_on(async { UnixListener::bind(&socket) })?;
        let asked = Arc::new(AtomicUsize::new(0));
        let counting = Arc::clone(&asked);
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
                    Ok(ToServer::Hello { .. }) => Ok(vec![ToClient::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: "scripted".to_owned(),
                        capabilities: Capabilities::from_bits(0),
                        instance: None,
                        build: None,
                    }]),
                    Ok(ToServer::SnapshotRequest) => {
                        answer(counting.fetch_add(1, Ordering::AcqRel) == 0)
                    }
                    Ok(_otherwise) => continue,
                    Err(_unreadable) => return,
                };
                let Ok(said) = said else { return };
                for message in said {
                    let Ok(bytes) = encode_to_client(&message) else {
                        return;
                    };
                    if link.send(CHANNEL_CONTROL, &bytes).await.is_err() {
                        return;
                    }
                }
            }
        });
        let artifacts = held.path.join("artifacts");
        std::fs::create_dir_all(&artifacts)?;
        let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
        let mut options = ManagerOptions::new(artifacts, paths);
        options.channel = ChannelOptions {
            ping_interval: Duration::from_millis(50),
            pong_deadline: Duration::from_secs(5),
            open_deadline: Duration::from_secs(5),
            greeting_deadline: Duration::from_secs(1),
        };
        let manager = HostManager::new(options)?;
        manager.add_host(&format!("{LOCAL_PREFIX}{}", socket.display()))?;
        // The launch asks once; the first gap asks a second time.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        while asked.load(Ordering::Acquire) < 2 && Instant::now() < expires {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(SETTLE);
        assert_eq!(
            asked.load(Ordering::Acquire),
            2,
            "one asking for the connection and one for the whole run of gaps"
        );
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
