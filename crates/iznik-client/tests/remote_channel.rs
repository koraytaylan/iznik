//! The channel over a local transport, against a real daemon and against a
//! server written to misbehave.
//!
//! `unix:<path>` is the alias this crate owns, and it is what makes these cases
//! possible without SSH: a daemon on this machine, a socket, and the same
//! channel the SSH path would build. What needs a network — the handshake
//! through `iznik-server --stdio` over real SSH, and a link that dies while a
//! pane is subscribed — is a scenario in the container.
//!
//! Every timing here is in the hundreds of milliseconds, because a case about
//! a dead link that took ten seconds to say so would be a case nobody runs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iznik_client::transport::channel::{ChannelError, ChannelOptions, RemoteChannel};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX, Transport};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::net::{UnixListener, UnixStream};

/// How long these cases give a channel that should answer at once.
const PROMPT: Duration = Duration::from_secs(5);

/// How often the liveness case pings.
const QUICK_PING: Duration = Duration::from_millis(50);

/// How long it lets silence last.
const QUICK_PONG: Duration = Duration::from_millis(200);

/// How long the whole liveness case may take: the pong deadline and a ping
/// interval, with room for a loaded machine, and still under a second.
const LIVENESS_CEILING: Duration = Duration::from_millis(900);

/// A version no server speaks, for the mismatch case.
const OTHER_VERSION: u16 = PROTOCOL_VERSION.saturating_add(1);

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
/// When the directory cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-channel-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// The transport that reaches `socket`, with this case's own runtime paths.
///
/// # Errors
///
/// When the runtime paths cannot be made.
fn local(held: &Scratch, socket: &std::path::Path) -> Result<Transport, Failed> {
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    Ok(Transport::for_alias(
        &format!("{LOCAL_PREFIX}{}", socket.display()),
        &paths,
        SshOptions::default(),
    ))
}

/// Options with the two liveness timings shortened.
fn brisk() -> ChannelOptions {
    ChannelOptions {
        ping_interval: QUICK_PING,
        pong_deadline: QUICK_PONG,
        ..ChannelOptions::default()
    }
}

/// Answers one connection with a `Hello` of `version`, then does what `after`
/// says with the link.
///
/// The listener is bound before this returns, so a caller may connect at once.
///
/// # Errors
///
/// When the socket cannot be bound.
fn scripted(
    socket: &std::path::Path,
    version: u16,
    capabilities: Capabilities,
    stay: bool,
) -> Result<tokio::task::JoinHandle<()>, Failed> {
    let listener = UnixListener::bind(socket)?;
    Ok(tokio::spawn(async move {
        let Ok((stream, _from)) = listener.accept().await else {
            return;
        };
        let mut link = FramedLink::new(stream);
        // The client's `Hello` first, because a server that answered before
        // hearing would not be the server this client talks to.
        let Ok(Some(frame)) = link.next_frame().await else {
            return;
        };
        if decode_to_server(frame.payload).is_err() {
            return;
        }
        let Ok(hello) = encode_to_client(&ToClient::Hello {
            protocol_version: version,
            server_version: "scripted".to_owned(),
            capabilities,
        }) else {
            return;
        };
        let _sent = link.send(CHANNEL_CONTROL, &hello).await;
        if stay {
            // Held open and silent: what a link that has stopped answering
            // looks like from the inside.
            std::future::pending::<()>().await;
        }
    }))
}

/// # Panics
///
/// When a channel to a real daemon does not shake hands, or does not agree
/// with it about compression.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_shakes_hands_with_the_daemon() {
    let case = async {
        let held = scratch("handshake")?;
        let stack = Stack::start(StackOptions::default()).await?;
        let transport = local(&held, stack.socket())?;
        let channel = RemoteChannel::open(&transport, None, ChannelOptions::default()).await?;
        let greeting = channel.greeting();
        assert_eq!(
            greeting.protocol_version, PROTOCOL_VERSION,
            "the server speaks what this client does, or opening would have refused"
        );
        // Every crate in this workspace takes its version from the workspace,
        // so the version the server says is the one this test was built at.
        assert_eq!(
            greeting.server_version,
            env!("CARGO_PKG_VERSION"),
            "and says which version it is"
        );
        assert!(
            greeting.capabilities.bits() & Capabilities::ZSTD.bits() != 0,
            "and offers compression, which this client asked for"
        );
        channel.close();
        drop(stack);
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a server speaking another protocol version is not refused by name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_refuses_another_protocol_version() {
    let case = async {
        let held = scratch("version")?;
        let socket = held.path.join("scripted.sock");
        let answering = scripted(&socket, OTHER_VERSION, Capabilities::from_bits(0), true)?;
        let transport = local(&held, &socket)?;
        let refused = RemoteChannel::open(&transport, None, brisk()).await;
        answering.abort();
        let Err(ChannelError::ProtocolVersion { host, server }) = refused else {
            return Err(format!("another version was not refused: {refused:?}").into());
        };
        assert_eq!(server, OTHER_VERSION, "and says which version it speaks");
        assert!(host.contains("scripted.sock"), "and which host: {host}");
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a link that has stopped answering is not reported dead inside the
/// deadline it was given.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_link_is_dead_within_its_deadline() {
    let case = async {
        let held = scratch("dead")?;
        let socket = held.path.join("silent.sock");
        let answering = scripted(&socket, PROTOCOL_VERSION, Capabilities::from_bits(0), true)?;
        let transport = local(&held, &socket)?;
        let mut channel = RemoteChannel::open(&transport, None, brisk()).await?;
        let started = Instant::now();
        // A deadline far past the pong deadline, so what answers is the
        // liveness and not the caller's own patience.
        let waited = channel
            .next(started.checked_add(PROMPT).unwrap_or(started))
            .await;
        let taken = started.elapsed();
        answering.abort();
        let Err(ChannelError::Dead { host, silent_for }) = waited else {
            return Err(format!("a silent link was not called dead: {waited:?}").into());
        };
        assert!(host.contains("silent.sock"), "and names the host: {host}");
        assert!(
            silent_for >= QUICK_PONG,
            "and how long it was silent: {silent_for:?}"
        );
        assert!(
            taken < LIVENESS_CEILING,
            "and says so in {LIVENESS_CEILING:?}, not after a transport timeout: {taken:?}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a channel over a socket cannot carry a command and its answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_carries_what_the_server_answers() {
    let case = async {
        let held = scratch("carries")?;
        let stack = Stack::start(StackOptions::default()).await?;
        let transport = local(&held, stack.socket())?;
        let mut channel = RemoteChannel::open(&transport, None, ChannelOptions::default()).await?;
        let asked = iznik_protocol::message::encode_to_server(&ToServer::SnapshotRequest)?;
        channel.send(CHANNEL_CONTROL, &asked).await?;
        let started = Instant::now();
        let answer = channel
            .next(started.checked_add(PROMPT).unwrap_or(started))
            .await?;
        assert_eq!(
            answer.channel, CHANNEL_CONTROL,
            "the answer comes on the control channel"
        );
        let message = iznik_protocol::message::decode_to_client(&answer.payload)?;
        assert!(
            matches!(message, ToClient::Snapshot { .. }),
            "and is the snapshot that was asked for: {message:?}"
        );
        channel.close();
        drop(stack);
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// Keeps the unused import honest: a socket is what a local transport reaches.
const _: fn() -> Option<UnixStream> = || None;
