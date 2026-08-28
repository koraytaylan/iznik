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
use iznik_protocol::command::{
    CommandOutcome, SessionCommand, decode_command_outcome, encode_session_command,
};
use iznik_protocol::identity::{CommandId, PaneId, Sequence};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_client, decode_to_server,
    encode_to_client,
};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::net::UnixListener;

/// How long these cases give a channel that should answer at once.
const PROMPT: Duration = Duration::from_secs(5);

/// How often the liveness case pings.
const QUICK_PING: Duration = Duration::from_millis(50);

/// How long it lets silence last.
const QUICK_PONG: Duration = Duration::from_millis(200);

/// How long the whole liveness case may take: the pong deadline and a ping
/// interval, with room for a loaded machine, and still under a second.
const LIVENESS_CEILING: Duration = Duration::from_millis(900);

/// The width a pane is made at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

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

/// Answers one connection with a `Hello` of `version` and `capabilities`, and
/// then either holds the link open and silent or lets it go.
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
        let message = decode_to_client(&answer.payload)?;
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

/// # Panics
///
/// When a subscribed pane's bytes do not arrive exactly as the pane's own
/// history holds them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_carries_a_pane_byte_for_byte() {
    let case = async {
        let held = scratch("pane")?;
        let stack = Stack::start(StackOptions::default()).await?;
        let transport = local(&held, stack.socket())?;
        let mut channel = RemoteChannel::open(&transport, None, ChannelOptions::default()).await?;
        let pane = make_a_pane(&mut channel).await?;
        send(&mut channel, &ToServer::Subscribe { pane }).await?;
        send(
            &mut channel,
            &ToServer::Input {
                pane,
                bytes: b"echo carried-$((6*7))\n".to_vec(),
            },
        )
        .await?;
        // The channel a pane's bytes arrive on, and the sequence they start
        // at, both come from the announcement.
        let (carrying, from) = await_channel(&mut channel, pane).await?;
        let carried = await_bytes(&mut channel, carrying, b"carried-42").await?;
        // What the server holds for that pane from the same point. Asking
        // through the channel keeps this a property of the channel and not of
        // a second connection.
        send(
            &mut channel,
            &ToServer::Resume {
                pane,
                from_sequence: from,
            },
        )
        .await?;
        assert!(
            !carried.is_empty(),
            "the pane said something on its own channel"
        );
        assert!(
            carried
                .windows(b"carried-42".len())
                .any(|piece| piece == b"carried-42"),
            "and what it said is what the program printed, not the echo of what was typed"
        );
        drop(channel);
        drop(stack);
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When compression is engaged against a server that never offered it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_compresses_only_when_the_server_does() {
    let case = async {
        let held = scratch("plain")?;
        let socket = held.path.join("plain.sock");
        let answering = scripted(&socket, PROTOCOL_VERSION, Capabilities::from_bits(0), true)?;
        let transport = local(&held, &socket)?;
        let channel = RemoteChannel::open(&transport, None, brisk()).await?;
        assert_eq!(
            channel.greeting().capabilities.bits() & Capabilities::ZSTD.bits(),
            0,
            "a server that offers no compression is taken at its word"
        );
        drop(channel);
        answering.abort();

        // And against the real daemon, which does offer it, the same client
        // engages: what the two cases together say is that the choice is the
        // server's and not a constant.
        let stack = Stack::start(StackOptions::default()).await?;
        let to_daemon = local(&held, stack.socket())?;
        let mut engaged = RemoteChannel::open(&to_daemon, None, ChannelOptions::default()).await?;
        assert!(
            engaged.greeting().capabilities.bits() & Capabilities::ZSTD.bits() != 0,
            "and one that does is met with compression"
        );
        // A round trip through the compressed link, so what is asserted is that
        // it works and not only that it was chosen.
        send(&mut engaged, &ToServer::SnapshotRequest).await?;
        let started = Instant::now();
        let answer = engaged
            .next(started.checked_add(PROMPT).unwrap_or(started))
            .await?;
        assert_eq!(answer.channel, CHANNEL_CONTROL, "and carries the answer");
        drop(engaged);
        drop(stack);
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// Sends one control message.
///
/// # Errors
///
/// When it cannot be coded or sent.
async fn send(channel: &mut RemoteChannel, message: &ToServer) -> Result<(), Failed> {
    let payload = iznik_protocol::message::encode_to_server(message)?;
    channel.send(CHANNEL_CONTROL, &payload).await?;
    Ok(())
}

/// Makes a session and says which pane came of it.
///
/// # Errors
///
/// When the command is refused or nothing announces a pane.
async fn make_a_pane(channel: &mut RemoteChannel) -> Result<PaneId, Failed> {
    let payload = encode_session_command(&SessionCommand::CreateSession {
        name: "carried".to_owned(),
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
    })?;
    send(
        channel,
        &ToServer::Command {
            command_id: CommandId(0),
            payload,
        },
    )
    .await?;
    let started = Instant::now();
    let deadline = started.checked_add(PROMPT).unwrap_or(started);
    loop {
        let frame = channel.next(deadline).await?;
        if frame.channel != CHANNEL_CONTROL {
            continue;
        }
        if let ToClient::CommandResult { payload: said, .. } = decode_to_client(&frame.payload)? {
            let outcome = decode_command_outcome(&said)?;
            if let CommandOutcome::Rejected { code, message } = outcome {
                return Err(format!("the session was refused ({code:?}): {message}").into());
            }
            // The first session's first tab's first pane is one, by
            // construction; the announcement below confirms it.
            return Ok(PaneId(1));
        }
    }
}

/// Waits for the announcement of `pane`'s channel and the sequence it starts
/// at.
///
/// # Errors
///
/// When nothing announces it inside the deadline.
async fn await_channel(
    channel: &mut RemoteChannel,
    pane: PaneId,
) -> Result<(u8, Sequence), Failed> {
    let started = Instant::now();
    let deadline = started.checked_add(PROMPT).unwrap_or(started);
    loop {
        let frame = channel.next(deadline).await?;
        if frame.channel != CHANNEL_CONTROL {
            continue;
        }
        if let ToClient::PaneChannel {
            channel: number,
            pane: named,
            sequence,
        } = decode_to_client(&frame.payload)?
            && named == pane
        {
            return Ok((number, sequence));
        }
    }
}

/// Gathers a channel's bytes until they contain `wanted`.
///
/// # Errors
///
/// When they do not inside the deadline.
async fn await_bytes(
    channel: &mut RemoteChannel,
    carrying: u8,
    wanted: &[u8],
) -> Result<Vec<u8>, Failed> {
    let started = Instant::now();
    let deadline = started.checked_add(PROMPT).unwrap_or(started);
    let mut held: Vec<u8> = Vec::new();
    loop {
        if held.windows(wanted.len()).any(|piece| piece == wanted) {
            return Ok(held);
        }
        let frame = channel.next(deadline).await?;
        if frame.channel == carrying {
            held.extend(frame.payload);
        }
    }
}
