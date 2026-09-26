//! A daemon that names the build it runs is judged by it.
//!
//! Every build of one version gives the same version, so a daemon still
//! running another build over this build's binary is told apart only by the
//! digest it read beside its binary when it started. The same digest as the
//! server this build carries is this build: its capabilities are read and
//! nothing is offered. Another is another build: the bits a command is gated
//! on are not this build's to read, the adoption bit is kept so an upgrade
//! can offer to keep the sessions, and replacing it is offered.

use core::time::Duration;
use std::path::PathBuf;
use std::time::Instant;

use iznik_client::bootstrap::upload::BINARY_NAME;
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::{HostState, UpgradeOffer, UpgradeReason};
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{BuildDigest, DaemonInstance, Generation};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, encode_host_model};
use tokio::net::UnixListener;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// How long a case waits for the host to be connected.
const PROMPT: Duration = Duration::from_secs(10);

/// The bytes of the server this build carries, in these cases.
const CARRIED: &[u8] = b"the server this build carries";

/// The triple it is carried for.
const TRIPLE: &str = "x86_64-unknown-linux-musl";

/// The daemon the host runs.
const RUNNING: DaemonInstance = DaemonInstance(0x0bad);

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

/// A scratch directory named for `case`, with the carried server under
/// `artifacts`.
///
/// # Errors
///
/// When it cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-build-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    let carried = path.join("artifacts").join(TRIPLE);
    std::fs::create_dir_all(&carried)?;
    std::fs::write(carried.join(BINARY_NAME), CARRIED)?;
    Ok(Scratch { path })
}

/// The digest of some bytes.
fn digest_of(bytes: &[u8]) -> BuildDigest {
    use sha2::Digest as _;
    BuildDigest(sha2::Sha256::digest(bytes).into())
}

/// A host of this build's version and every capability but compression,
/// whose daemon says it runs `build`.
///
/// # Errors
///
/// When the socket cannot be bound.
fn host(runtime: &Runtime, socket: &std::path::Path, build: BuildDigest) -> Result<(), Failed> {
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
                Ok(ToServer::Hello { .. }) => ToClient::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    // Every one but compression, which this host does not speak.
                    capabilities: Capabilities::from_bits(
                        Capabilities::known().bits() & !Capabilities::ZSTD.bits(),
                    ),
                    instance: Some(RUNNING),
                    build: Some(build),
                },
                Ok(ToServer::SnapshotRequest) => {
                    let model = HostModel {
                        generation: Generation(1),
                        sessions: Vec::new(),
                    };
                    let Ok(payload) = encode_host_model(&model) else {
                        return;
                    };
                    ToClient::Snapshot {
                        generation: model.generation,
                        payload,
                    }
                }
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
    });
    Ok(())
}

/// Connects to a host whose daemon says it runs `build`, and gives back what
/// the connection trusted of its capabilities and what it offered.
///
/// # Errors
///
/// When the host is never connected.
fn connected(
    case: &str,
    build: BuildDigest,
) -> Result<(Capabilities, Option<UpgradeOffer>), Failed> {
    let held = scratch(case)?;
    let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
    let socket = held.path.join("scripted.sock");
    host(&runtime, &socket, build)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let manager = HostManager::new(ManagerOptions::new(held.path.join("artifacts"), paths))?;
    let events = manager.events();
    manager.add_host(&format!("{LOCAL_PREFIX}{}", socket.display()))?;
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        if let ManagerEvent::Moved {
            state:
                HostState::Connected {
                    capabilities,
                    upgrade,
                    ..
                },
            ..
        } = event
        {
            drop(manager);
            return Ok((capabilities, upgrade));
        }
    }
    Err("the host was never connected".into())
}

/// # Panics
///
/// When a daemon that runs the very server this build carries is offered a
/// replacement, or has its capabilities dropped.
#[test]
fn build_digest_of_this_build_is_trusted_and_offered_nothing() {
    let (capabilities, offer) =
        connected("same", digest_of(CARRIED)).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(offer, None, "this build is offered nothing: {offer:?}");
    assert!(
        capabilities.contains(Capabilities::REORDER_SESSIONS),
        "and what it advertised is read: {capabilities:?}"
    );
}

/// # Panics
///
/// When a daemon that says it runs another build of this version is trusted
/// as this one, or not offered its replacement.
#[test]
fn build_digest_of_another_build_is_offered_a_replacement() {
    let (capabilities, offer) =
        connected("other", digest_of(b"another build")).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        offer.map(|offered| offered.reason),
        Some(UpgradeReason::Build),
        "another build is offered its replacement, for the build"
    );
    assert_eq!(
        capabilities,
        Capabilities::ADOPT,
        "feature bits of another build stay unread; adoption is what the upgrade choice reads"
    );
}
