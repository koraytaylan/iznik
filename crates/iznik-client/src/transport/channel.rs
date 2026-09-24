//! One channel per host carrying every pane: the `Hello` exchange, compression, liveness pings, and the deadline that surfaces a dead link at once.
//!
//! One channel, not many. Several SSH channels would share one TCP connection
//! and inherit its head-of-line blocking with none of the multiplexer's
//! control over who goes next — so every pane, every command and every delta
//! travels the one link the server already schedules.
//!
//! Liveness is why this is not just a link. `ServerAliveInterval` notices a
//! host that has stopped answering at the transport's own pace, which is tens
//! of seconds; a person whose laptop woke on a different network wants to be
//! told sooner, and a client that waits out a TCP timeout looks broken rather
//! than disconnected. So a `Ping` goes out on its own task, and a channel that
//! has heard nothing for the pong deadline says so.

use core::fmt::{self, Display, Formatter};
use core::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iznik_link::compression::compressed;
use iznik_link::framed::{FrameReader, FrameWriter, FramedLink, LinkError};
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{BuildDigest, DaemonInstance};
use iznik_protocol::message::{
    CHANNEL_CONTROL, ErrorCode, MessageError, PROTOCOL_VERSION, RELAY_READY, ToClient, ToServer,
    decode_to_client, encode_to_server,
};
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::transport::Transport;
use crate::transport::ssh::{SshChild, SshError};

/// How often a channel asks the server whether it is still there.
pub const PING_INTERVAL: Duration = Duration::from_secs(5);

/// How long it may hear nothing at all before the link is dead.
pub const PONG_DEADLINE: Duration = Duration::from_secs(10);

/// How long getting a link may take: the transport and the remote command.
pub const OPEN_DEADLINE: Duration = Duration::from_secs(30);

/// How long a server that is there has to say hello.
///
/// One round trip on a link that is already up, so far shorter than the
/// opening: a server that has started and not greeted in this long is one that
/// is not going to.
pub const GREETING_DEADLINE: Duration = Duration::from_secs(10);

/// How many bytes of the remote's complaints are kept. A host that says
/// nothing useful in its first kibibyte is a host whose message was not the
/// point; what matters is that something is kept and that the pipe is read.
const REMOTE_COMPLAINT_BYTES: usize = 1024;

/// The least a ping interval may be. A scenario sets these in milliseconds and
/// a zero would make the liveness task a loop with nothing in it, saturating a
/// core and flooding the server with pings; ten milliseconds is far below any
/// deadline worth setting and still an interval.
const LEAST_PING_INTERVAL: Duration = Duration::from_millis(10);

/// What the bootstrap runs on a host, and what a channel talks to.
const STDIO_FLAG: &str = "--stdio";

/// The server a channel runs when it is not told which.
const DEFAULT_SERVER: &str = "iznik-server";

/// The command a channel asks a host to run.
///
/// Its standard input is the link, so unlike a bootstrap script it cannot be
/// read by `sh` from there: it is the one command the login shell itself
/// parses. So it is plain words whenever it can be — a path of letters,
/// digits and `/._-` reads the same to a Bourne shell, fish, nushell and a C
/// shell. A path with anything else in it is quoted, because it is a path the
/// *host* chose: the probe offers `$XDG_DATA_HOME` and `$TMPDIR` among its
/// candidates and both are whatever somebody set them to, and an unquoted
/// `/mnt/My Data/iznik/bin/iznik-server` would be split into a program that
/// does not exist and two arguments.
#[must_use]
pub fn relay_command(server: Option<&Path>) -> String {
    let named = server
        .unwrap_or_else(|| Path::new(DEFAULT_SERVER))
        .display()
        .to_string();
    let plain = named
        .chars()
        .all(|held| held.is_ascii_alphanumeric() || matches!(held, '/' | '.' | '_' | '-'));
    if plain {
        format!("{named} {STDIO_FLAG}")
    } else {
        format!("{} {STDIO_FLAG}", crate::bootstrap::upload::quoted(&named))
    }
}

/// Every timing a channel runs under, so a test can shorten any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOptions {
    /// How often to ask whether the server is there.
    pub ping_interval: Duration,
    /// How long silence may last before the link is dead.
    pub pong_deadline: Duration,
    /// How long getting a link may take — over `ssh`, until the relay on the
    /// host says it has reached the daemon, which covers `ssh` connecting and
    /// authenticating and the daemon starting.
    pub open_deadline: Duration,
    /// How long the server has to greet once there is one.
    ///
    /// Its own, rather than whatever the opening did not spend: a dial that
    /// took most of its deadline would otherwise leave a healthy but slow
    /// server no time at all, and be reported as one that said nothing. Over
    /// `ssh` it runs from the relay's [`RELAY_READY`] line, not from when
    /// `ssh` was started.
    pub greeting_deadline: Duration,
}

impl Default for ChannelOptions {
    fn default() -> ChannelOptions {
        ChannelOptions {
            ping_interval: PING_INTERVAL,
            pong_deadline: PONG_DEADLINE,
            open_deadline: OPEN_DEADLINE,
            greeting_deadline: GREETING_DEADLINE,
        }
    }
}

/// A duplex byte stream, whichever kind of transport it came from.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}

impl<Stream: AsyncRead + AsyncWrite + Unpin + Send> Duplex for Stream {}

/// The stream under a channel. Boxed because a channel over `ssh` and a channel
/// over a socket are two types and everything above them is one.
type Wire = Box<dyn Duplex>;

/// Why a channel could not do something.
#[derive(Debug)]
pub enum ChannelError {
    /// The transport could not be started.
    Transport(SshError),
    /// The link failed.
    Link(LinkError),
    /// A message could not be encoded or decoded.
    Message(MessageError),
    /// The stream could not be opened, or compression could not be engaged.
    Io {
        /// The host.
        host: String,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The server speaks another version of the protocol, and the daemon *is*
    /// the sessions — so this is reported, never worked around.
    ProtocolVersion {
        /// The host.
        host: String,
        /// What it said it speaks, when it said a number: a server that
        /// refuses this client's version says so in words, which carry it.
        server: Option<u16>,
    },
    /// The link came up and the server never greeted.
    Silent {
        /// The host.
        host: String,
        /// How long it was given to say hello.
        waited: Duration,
    },
    /// Nothing has arrived for longer than the pong deadline.
    Dead {
        /// The host.
        host: String,
        /// How long it has been silent.
        silent_for: Duration,
    },
    /// The caller's own deadline passed with nothing to give it.
    Deadline {
        /// The host.
        host: String,
        /// How long it waited.
        waited: Duration,
    },
    /// The server closed the link.
    Closed {
        /// The host.
        host: String,
        /// What the remote said on its standard error before it went, which is
        /// usually the whole reason.
        said: String,
    },
    /// Something arrived that has no place here.
    Unexpected {
        /// The host.
        host: String,
        /// What was wanted.
        wanted: &'static str,
        /// What came instead.
        received: String,
    },
}

impl Display for ChannelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ChannelError::Transport(source) => write!(formatter, "{source}"),
            ChannelError::Link(source) => write!(formatter, "the link failed: {source}"),
            ChannelError::Message(source) => write!(formatter, "the message failed: {source}"),
            ChannelError::Io { host, source } => write!(formatter, "{host}: {source}"),
            ChannelError::ProtocolVersion {
                host,
                server: Some(server),
            } => write!(
                formatter,
                "{host} speaks iznik/{server} and this client speaks iznik/{PROTOCOL_VERSION}. \
                 The server holds the sessions, so it is not replaced without being asked."
            ),
            ChannelError::ProtocolVersion { host, server: None } => write!(
                formatter,
                "{host} speaks another version of iznik's protocol than this client's \
                 iznik/{PROTOCOL_VERSION}. The server holds the sessions, so it is not replaced \
                 without being asked."
            ),
            ChannelError::Silent { host, waited } => write!(
                formatter,
                "{host} started a server and it said nothing in {waited:?}"
            ),
            ChannelError::Dead { host, silent_for } => write!(
                formatter,
                "{host} has said nothing for {silent_for:?}; the link is gone"
            ),
            ChannelError::Deadline { host, waited } => {
                write!(formatter, "{host} said nothing within {waited:?}")
            }
            ChannelError::Closed { host, said } if said.is_empty() => {
                write!(formatter, "{host} closed the link")
            }
            ChannelError::Closed { host, said } => {
                write!(formatter, "{host} closed the link: {said}")
            }
            ChannelError::Unexpected {
                host,
                wanted,
                received,
            } => write!(
                formatter,
                "{host} sent {received} where {wanted} was wanted"
            ),
        }
    }
}

impl core::error::Error for ChannelError {}

/// What the server said in its half of the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerHello {
    /// The protocol version it speaks, which matched this client's.
    pub protocol_version: u16,
    /// Its own version, for logs and for the upgrade decision.
    pub server_version: String,
    /// What it can do.
    pub capabilities: Capabilities,
    /// Which run of its daemon answered, when it says.
    ///
    /// A server that does not is one built before it could, and a client can
    /// then only guess whether it is talking to the daemon it last spoke to.
    pub instance: Option<DaemonInstance>,
    /// Which build of the server its daemon runs, when it says: the digest
    /// of the binary the daemon was started from.
    ///
    /// Every build of one version gives the same version, so this is the one
    /// thing that says whether the daemon answering is this build's own.
    pub build: Option<BuildDigest>,
}

/// A stream to the server, before anything has been said on it.
struct Dialed {
    /// The link over it.
    link: FramedLink<Wire>,
    /// The `ssh` it goes through, when it goes through one.
    child: Option<SshChild>,
    /// What the remote says on its standard error, kept as it arrives.
    complaints: Arc<Mutex<String>>,
    /// Raised when the relay on the host says it has reached the daemon;
    /// `None` for a socket on this machine, which is reached when it connects.
    relayed: Option<Arc<Notify>>,
}

/// Waits for a server's greeting under the two deadlines an opening has.
///
/// With `relayed` — a link through `ssh` — the connection is still being made
/// until the relay says it has reached the daemon: `ssh` connecting and
/// authenticating, the daemon starting. That has `opening` to happen in, and
/// running out of it is a link that never came up
/// ([`ChannelError::Deadline`]), not a server that would not speak. Only once
/// the relay is up does the server have `greeting` to answer in, and running
/// out of that is [`ChannelError::Silent`]. A greeting that arrives before the
/// relay says so — from a server built before it did — is taken as it comes.
///
/// # Errors
///
/// [`ChannelError::Deadline`] or [`ChannelError::Silent`] as above, and
/// whatever `greeted` itself fails with.
pub async fn await_greeting<Greeted>(
    greeted: impl Future<Output = Result<Greeted, ChannelError>>,
    relayed: Option<&Notify>,
    host: &str,
    opening: Duration,
    greeting: Duration,
) -> Result<Greeted, ChannelError> {
    let greeted = core::pin::pin!(greeted);
    let mut greeted = greeted;
    if let Some(relayed) = relayed {
        tokio::select! {
            done = greeted.as_mut() => return done,
            () = relayed.notified() => {}
            () = tokio::time::sleep(opening) => {
                return Err(ChannelError::Deadline {
                    host: host.to_owned(),
                    waited: opening,
                });
            }
        }
    }
    tokio::time::timeout(greeting, greeted)
        .await
        .unwrap_or_else(|_elapsed| {
            Err(ChannelError::Silent {
                host: host.to_owned(),
                waited: greeting,
            })
        })
}

/// One frame, owned, because the reader lends its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    /// The channel it came in on.
    pub channel: u8,
    /// Its bytes.
    pub payload: Vec<u8>,
}

/// One channel to one host.
pub struct RemoteChannel {
    /// The host, for everything that must name it.
    host: String,
    /// The reading half.
    reader: FrameReader<Wire>,
    /// The writing half, shared with the liveness task.
    writer: Arc<Mutex<FrameWriter<Wire>>>,
    /// When something last arrived.
    heard: Instant,
    /// The liveness task, ended when this is dropped.
    liveness: JoinHandle<()>,
    /// What the remote said on its standard error, kept as it arrives.
    ///
    /// Read by a task of its own for two reasons. A host with no server at the
    /// path it was given says `command not found` there and nothing on its
    /// output, so a channel that did not read this could only report that the
    /// link closed — and this plan requires an error to carry what the remote
    /// said. And nobody reading it at all means a long session fills the pipe
    /// and stops the remote process inside a write.
    complaints: Arc<Mutex<String>>,
    /// The `ssh` this speaks through, when it speaks through one.
    ///
    /// Held, not dropped: the transport spawns with `kill_on_drop`, so letting
    /// the child go after taking its pipes would kill the very process the
    /// pipes lead to — which reads, from the other end, exactly like a host
    /// that closed the link.
    child: Option<SshChild>,
    /// The timings.
    options: ChannelOptions,
    /// What the server said when it opened.
    greeting: ServerHello,
}

impl fmt::Debug for RemoteChannel {
    /// The host and what it said; the stream under it is a boxed trait object
    /// with nothing to show.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteChannel")
            .field("host", &self.host)
            .field("over_ssh", &self.child.is_some())
            .field("greeting", &self.greeting)
            .finish_non_exhaustive()
    }
}

impl Drop for RemoteChannel {
    fn drop(&mut self) {
        self.liveness.abort();
        // And the `ssh` goes with it, because the transport spawned it with
        // `kill_on_drop`. Saying so here is what keeps the held child from
        // looking like something nobody uses.
        drop(self.child.take());
    }
}

/// The link closed, carrying whatever the remote had said about it.
fn closed(host: &str, said: &str) -> ChannelError {
    ChannelError::Closed {
        host: host.to_owned(),
        said: said.trim().to_owned(),
    }
}

/// Watches a remote's standard error, piece by piece, for the relay's
/// [`RELAY_READY`] line — wherever it falls, however much came before it, and
/// however the pieces cut it.
#[derive(Clone, Debug, Default)]
pub struct ReadyWatch {
    /// The end of what has been heard: as much of it as could still be the
    /// beginning of the line, and never the whole line.
    tail: String,
}

impl ReadyWatch {
    /// Takes the next piece of standard error, and says whether the ready
    /// line was completed in it.
    pub fn heard(&mut self, said: &str) -> bool {
        let line = format!("{RELAY_READY}\n");
        let mut window = core::mem::take(&mut self.tail);
        window.push_str(said);
        let found = window.contains(&line);
        // One byte short of the line, so the line is never heard twice.
        let mut from = window.len().saturating_sub(line.len().saturating_sub(1));
        while !window.is_char_boundary(from) {
            from = from.saturating_add(1);
        }
        window
            .get(from..)
            .unwrap_or_default()
            .clone_into(&mut self.tail);
        found
    }
}

/// Reads the remote's standard error into `kept` until it ends, holding the
/// first [`REMOTE_COMPLAINT_BYTES`] of it — and raises `relayed` when the
/// relay's [`RELAY_READY`] line comes, however much came before it; the line is
/// not a complaint, and is taken out of what is kept.
///
/// The first bytes and not the last: what a remote says before it goes is the
/// reason, and what it says afterwards is consequence.
fn keep_complaints(
    errors: tokio::process::ChildStderr,
    kept: Arc<Mutex<String>>,
    relayed: Arc<Notify>,
) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut errors = errors;
        let mut buffer = [0_u8; REMOTE_COMPLAINT_BYTES];
        let mut watch = ReadyWatch::default();
        loop {
            match errors.read(&mut buffer).await {
                Ok(0) => return,
                Err(_gone) => return,
                Ok(count) => {
                    let said = String::from_utf8_lossy(buffer.get(..count).unwrap_or_default());
                    // Scanned before anything is capped: a host whose login
                    // banner runs past what is kept still has its relay heard.
                    let ready = watch.heard(&said);
                    let mut held = kept.lock().await;
                    if held.len() < REMOTE_COMPLAINT_BYTES {
                        held.push_str(&said);
                    }
                    let line = format!("{RELAY_READY}\n");
                    if let Some(at) = held.find(&line) {
                        held.replace_range(at..at.saturating_add(line.len()), "");
                    }
                    drop(held);
                    if ready {
                        relayed.notify_one();
                    }
                }
            }
        }
    });
}

/// The protocol version a server's refusal names: the number its words end
/// with — "this server speaks protocol 2" — when they end with one.
fn spoken(message: &str) -> Option<u16> {
    let digits: String = message
        .trim_end()
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// Whether a capability set offers compression.
fn offers_zstd(capabilities: Capabilities) -> bool {
    capabilities.bits() & Capabilities::ZSTD.bits() != 0
}

/// What this client asks for: compression, resuming where it left off, to be
/// told which run of the daemon it reached and which build it runs, and how
/// far the host answered each pane's terminal queries itself.
fn wanted() -> Capabilities {
    Capabilities::from_bits(
        Capabilities::ZSTD.bits()
            | Capabilities::RESUME.bits()
            | Capabilities::INSTANCE.bits()
            | Capabilities::ANSWERED.bits()
            | Capabilities::BUILD.bits(),
    )
}

impl RemoteChannel {
    /// Opens a channel to the host `transport` names, running `server` there
    /// when the transport is `ssh` and connecting to the socket when it is not.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Transport`] when `ssh` cannot be started,
    /// [`ChannelError::Io`] when a socket cannot be reached,
    /// [`ChannelError::ProtocolVersion`] when the server speaks another
    /// version, and [`ChannelError::Deadline`] when the handshake does not
    /// finish inside [`ChannelOptions::open_deadline`].
    pub async fn open(
        transport: &Transport,
        server: Option<&Path>,
        options: ChannelOptions,
    ) -> Result<RemoteChannel, ChannelError> {
        let host = transport.alias();
        let waited = options.greeting_deadline;
        // The two halves are timed apart, because they fail for two different
        // reasons and a caller is told which. A link that never came up is a
        // path, a network or an `ssh` that could not start; a link that came
        // up and then said nothing is a server that is there and will not
        // speak. One deadline over both could only ever report the second.
        let dialing = RemoteChannel::dial(transport, server, host.clone());
        let dialed = tokio::time::timeout(options.open_deadline, dialing)
            .await
            .unwrap_or_else(|_elapsed| {
                Err(ChannelError::Deadline {
                    host: host.clone(),
                    waited: options.open_deadline,
                })
            })?;
        let opening = options.open_deadline;
        let relayed = dialed.relayed;
        let greeting = RemoteChannel::shake_hands(
            dialed.link,
            options,
            host.clone(),
            dialed.child,
            dialed.complaints,
        );
        await_greeting(greeting, relayed.as_deref(), &host, opening, waited).await
    }

    /// Connects to a daemon socket on this machine.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Io`] when the socket cannot be reached, and on an operating
    /// system without Unix sockets, because that alias is how tests reach a local
    /// daemon and a Windows client reaches hosts through `ssh`.
    async fn connect_local(socket: &Path, host: &str) -> Result<Wire, ChannelError> {
        #[cfg(unix)]
        {
            let stream = UnixStream::connect(socket)
                .await
                .map_err(|source| ChannelError::Io {
                    host: host.to_owned(),
                    source,
                })?;
            Ok(Box::new(stream))
        }
        #[cfg(not(unix))]
        {
            let _socket = socket;
            Err(ChannelError::Io {
                host: host.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "a unix: alias names a socket on this machine, and this operating system reaches hosts through ssh",
                ),
            })
        }
    }

    /// Gets a stream to the server, however this transport reaches one.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Transport`] when `ssh` cannot be started,
    /// [`ChannelError::Io`] when a socket cannot be reached, and
    /// [`ChannelError::Closed`] when the child has no streams to take.
    async fn dial(
        transport: &Transport,
        server: Option<&Path>,
        host: String,
    ) -> Result<Dialed, ChannelError> {
        let complaints = Arc::new(Mutex::new(String::new()));
        let mut relayed = None;
        let (wire, child): (Wire, Option<SshChild>) = match transport {
            Transport::Ssh(ssh) => {
                let command = relay_command(server);
                let mut spawned = ssh.spawn(&[command]).map_err(ChannelError::Transport)?;
                let stdin = spawned
                    .child
                    .stdin
                    .take()
                    .ok_or_else(|| closed(&host, ""))?;
                let stdout = spawned
                    .child
                    .stdout
                    .take()
                    .ok_or_else(|| closed(&host, ""))?;
                if let Some(errors) = spawned.child.stderr.take() {
                    let ready = Arc::new(Notify::new());
                    keep_complaints(errors, Arc::clone(&complaints), Arc::clone(&ready));
                    relayed = Some(ready);
                }
                (Box::new(tokio::io::join(stdout, stdin)), Some(spawned))
            }
            Transport::Local { socket } => (Self::connect_local(socket, &host).await?, None),
        };
        Ok(Dialed {
            link: FramedLink::new(wire),
            child,
            complaints,
            relayed,
        })
    }

    /// Says hello, hears the answer, and puts compression under the link when
    /// both ends offered it.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Link`] when the link fails, [`ChannelError::Message`]
    /// when the greeting cannot be coded, and whatever hearing it reports.
    async fn shake_hands(
        mut link: FramedLink<Wire>,
        options: ChannelOptions,
        host: String,
        child: Option<SshChild>,
        complaints: Arc<Mutex<String>>,
    ) -> Result<RemoteChannel, ChannelError> {
        let ours = wanted();
        let hello = encode_to_server(&ToServer::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_version: concat!("iznik-client ", env!("CARGO_PKG_VERSION")).to_owned(),
            capabilities: ours,
        })
        .map_err(ChannelError::Message)?;
        link.send(CHANNEL_CONTROL, &hello)
            .await
            .map_err(ChannelError::Link)?;
        let greeting = RemoteChannel::hear_hello(&mut link, &host, &complaints).await?;
        let link = if offers_zstd(ours) && offers_zstd(greeting.capabilities) {
            let (stream, leftover) = link.into_parts();
            // Through the shared layer, then boxed again so a compressed
            // channel and a plain one are one type from here on.
            let (engaged, pending) = compressed(stream, leftover)
                .map_err(|source| ChannelError::Io {
                    host: host.clone(),
                    source,
                })?
                .into_parts();
            let wire: Wire = Box::new(engaged);
            FramedLink::from_parts(wire, &pending)
        } else {
            link
        };
        Ok(RemoteChannel::running(
            link, options, host, greeting, child, complaints,
        ))
    }

    /// Reads the server's `Hello`, refusing another protocol version.
    ///
    /// # Errors
    ///
    /// [`ChannelError::ProtocolVersion`] when the versions differ,
    /// [`ChannelError::Closed`] when nothing comes, and
    /// [`ChannelError::Unexpected`] when something else does.
    async fn hear_hello(
        link: &mut FramedLink<Wire>,
        host: &str,
        complaints: &Arc<Mutex<String>>,
    ) -> Result<ServerHello, ChannelError> {
        let Some(frame) = link.next_frame().await.map_err(ChannelError::Link)? else {
            // The remote went before it said hello, and what it said on its
            // standard error is the reason — a server that is not where it was
            // said to be, most often.
            let said = complaints.lock().await.clone();
            return Err(closed(host, &said));
        };
        if frame.channel != CHANNEL_CONTROL {
            return Err(ChannelError::Unexpected {
                host: host.to_owned(),
                wanted: "the server's Hello on channel 0",
                received: format!("{} bytes on channel {}", frame.payload.len(), frame.channel),
            });
        }
        let message = decode_to_client(frame.payload).map_err(ChannelError::Message)?;
        let (protocol_version, server_version, capabilities, instance, build) = match message {
            ToClient::Hello {
                protocol_version,
                server_version,
                capabilities,
                instance,
                build,
            } => (
                protocol_version,
                server_version,
                capabilities,
                instance,
                build,
            ),
            // What a server of another version answers a `Hello` with: its
            // refusal, which is the version mismatch this client exists to
            // report by name — not something unexpected.
            ToClient::Error {
                code: ErrorCode::ProtocolVersion,
                message,
            } => {
                return Err(ChannelError::ProtocolVersion {
                    host: host.to_owned(),
                    server: spoken(&message),
                });
            }
            other => {
                return Err(ChannelError::Unexpected {
                    host: host.to_owned(),
                    wanted: "the server's Hello",
                    received: format!("{other:?}"),
                });
            }
        };
        if protocol_version != PROTOCOL_VERSION {
            return Err(ChannelError::ProtocolVersion {
                host: host.to_owned(),
                server: Some(protocol_version),
            });
        }
        Ok(ServerHello {
            protocol_version,
            server_version,
            capabilities,
            instance,
            build,
        })
    }

    /// Splits the link and starts the task that keeps asking.
    fn running(
        link: FramedLink<Wire>,
        options: ChannelOptions,
        host: String,
        greeting: ServerHello,
        child: Option<SshChild>,
        complaints: Arc<Mutex<String>>,
    ) -> RemoteChannel {
        let (reader, writer) = link.split();
        let writer = Arc::new(Mutex::new(writer));
        let heard = Instant::now();
        let asking = Arc::clone(&writer);
        let interval = options.ping_interval.max(LEAST_PING_INTERVAL);
        let liveness = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Ok(ping) = encode_to_server(&ToServer::Ping) else {
                    return;
                };
                // Never waits for the writer. A send parks when the peer has
                // stopped reading, and a ping that waited for the lock would
                // hold it for as long as that lasts — wedging every caller
                // that writes, on the one link they share, exactly when the
                // thing to do is notice. A writer that is busy is a writer
                // something else is using, which says as much as a pong.
                let Ok(mut held) = asking.try_lock() else {
                    continue;
                };
                if held.send(CHANNEL_CONTROL, &ping).await.is_err() {
                    // The link is gone; `next` is what says so, with how long
                    // it has been silent.
                    return;
                }
            }
        });
        RemoteChannel {
            host,
            reader,
            writer,
            heard,
            liveness,
            complaints,
            child,
            options,
            greeting,
        }
    }

    /// What the server said when this opened.
    #[must_use]
    pub fn greeting(&self) -> &ServerHello {
        &self.greeting
    }

    /// Sends one frame.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Link`] when the link fails.
    pub async fn send(&mut self, channel: u8, payload: &[u8]) -> Result<(), ChannelError> {
        self.writer
            .lock()
            .await
            .send(channel, payload)
            .await
            .map_err(ChannelError::Link)
    }

    /// The next frame that is not a `Pong`, or why there is none.
    ///
    /// Cancel-safe: once a deliverable frame is consumed, no operation can
    /// suspend before returning it. The manager races this against orders;
    /// even an uncontended async lock here could yield and lose that frame.
    ///
    /// This is the only thing that hears: the ping task asks, and what comes
    /// back is noticed here. A caller that stops calling it stops hearing, and
    /// will be told the link is dead when it next asks — which is right for
    /// the manager that runs it in a loop, and the only caller there is.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Dead`] when nothing has arrived for the pong deadline,
    /// [`ChannelError::Deadline`] when `deadline` passes first,
    /// [`ChannelError::Closed`] at a clean end, and [`ChannelError::Link`] when
    /// the link fails.
    pub async fn next(&mut self, deadline: Instant) -> Result<Received, ChannelError> {
        let asked_at = Instant::now();
        loop {
            // Before polling, not only around it: a reader with a frame always
            // ready wins the race against an expired deadline every time, and
            // a caller waiting for something a flooding pane never says would
            // wait for ever inside a call that was given a bound.
            if Instant::now() >= deadline {
                return Err(ChannelError::Deadline {
                    host: self.host.clone(),
                    waited: asked_at.elapsed(),
                });
            }
            let silent_since = self.heard;
            let dead_at = silent_since
                .checked_add(self.options.pong_deadline)
                .unwrap_or(silent_since);
            let until = deadline.min(dead_at);
            let frame = tokio::time::timeout_at(until.into(), self.reader.next_frame()).await;
            let Ok(frame) = frame else {
                if Instant::now() >= dead_at {
                    return Err(ChannelError::Dead {
                        host: self.host.clone(),
                        silent_for: silent_since.elapsed(),
                    });
                }
                return Err(ChannelError::Deadline {
                    host: self.host.clone(),
                    waited: asked_at.elapsed(),
                });
            };
            let frame = frame.map_err(ChannelError::Link)?;
            let Some(frame) = frame else {
                let said = self.complaints.lock().await.clone();
                return Err(closed(&self.host, &said));
            };
            let received = Received {
                channel: frame.channel,
                payload: frame.payload.to_vec(),
            };
            self.heard = Instant::now();
            if received.channel == CHANNEL_CONTROL
                && matches!(decode_to_client(&received.payload), Ok(ToClient::Pong))
            {
                // Heard, and not the caller's business.
                continue;
            }
            return Ok(received);
        }
    }

    /// Ends the channel: the liveness task stops and the link is dropped, which
    /// is what the server sees.
    pub fn close(self) {
        // `Drop` aborts the task; this exists so a caller can say when.
        drop(self);
    }
}
