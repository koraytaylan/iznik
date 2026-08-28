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
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iznik_link::compression::compressed;
use iznik_link::framed::{FrameReader, FrameWriter, FramedLink, LinkError};
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::message::{
    CHANNEL_CONTROL, MessageError, PROTOCOL_VERSION, ToClient, ToServer, decode_to_client,
    encode_to_server,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::transport::Transport;
use crate::transport::ssh::{SshChild, SshError};

/// How often a channel asks the server whether it is still there.
pub const PING_INTERVAL: Duration = Duration::from_secs(5);

/// How long it may hear nothing at all before the link is dead.
pub const PONG_DEADLINE: Duration = Duration::from_secs(10);

/// How long opening one may take: the transport, the remote command and the
/// handshake together.
pub const OPEN_DEADLINE: Duration = Duration::from_secs(30);

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

/// Every timing a channel runs under, so a test can shorten any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOptions {
    /// How often to ask whether the server is there.
    pub ping_interval: Duration,
    /// How long silence may last before the link is dead.
    pub pong_deadline: Duration,
    /// How long opening may take.
    pub open_deadline: Duration,
}

impl Default for ChannelOptions {
    fn default() -> ChannelOptions {
        ChannelOptions {
            ping_interval: PING_INTERVAL,
            pong_deadline: PONG_DEADLINE,
            open_deadline: OPEN_DEADLINE,
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
        /// What it said it speaks.
        server: u16,
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
            ChannelError::ProtocolVersion { host, server } => write!(
                formatter,
                "{host} speaks iznik/{server} and this client speaks iznik/{PROTOCOL_VERSION}. \
                 The server holds the sessions, so it is not replaced without being asked."
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
    heard: Arc<Mutex<Instant>>,
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

/// Reads the remote's standard error into `kept` until it ends, holding the
/// first [`REMOTE_COMPLAINT_BYTES`] of it.
///
/// The first bytes and not the last: what a remote says before it goes is the
/// reason, and what it says afterwards is consequence.
fn keep_complaints(errors: tokio::process::ChildStderr, kept: Arc<Mutex<String>>) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut errors = errors;
        let mut buffer = [0_u8; REMOTE_COMPLAINT_BYTES];
        loop {
            match errors.read(&mut buffer).await {
                Ok(0) => return,
                Err(_gone) => return,
                Ok(count) => {
                    let mut held = kept.lock().await;
                    if held.len() < REMOTE_COMPLAINT_BYTES {
                        let said = String::from_utf8_lossy(buffer.get(..count).unwrap_or_default());
                        held.push_str(&said);
                    }
                }
            }
        }
    });
}

/// Whether a capability set offers compression.
fn offers_zstd(capabilities: Capabilities) -> bool {
    capabilities.bits() & Capabilities::ZSTD.bits() != 0
}

/// What this client asks for: compression, and resuming where it left off.
fn wanted() -> Capabilities {
    Capabilities::from_bits(Capabilities::ZSTD.bits() | Capabilities::RESUME.bits())
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
        let opening = RemoteChannel::begin(transport, server, options.clone(), host.clone());
        tokio::time::timeout(options.open_deadline, opening)
            .await
            .unwrap_or_else(|_elapsed| {
                Err(ChannelError::Deadline {
                    host,
                    waited: options.open_deadline,
                })
            })
    }

    /// The opening itself, which [`RemoteChannel::open`] puts a deadline on.
    ///
    /// # Errors
    ///
    /// As [`RemoteChannel::open`], less the deadline it does not impose.
    async fn begin(
        transport: &Transport,
        server: Option<&Path>,
        options: ChannelOptions,
        host: String,
    ) -> Result<RemoteChannel, ChannelError> {
        let complaints = Arc::new(Mutex::new(String::new()));
        let (wire, child): (Wire, Option<SshChild>) = match transport {
            Transport::Ssh(ssh) => {
                let named = server.unwrap_or_else(|| Path::new("iznik-server"));
                let command = format!("{} {STDIO_FLAG}", named.display());
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
                    keep_complaints(errors, Arc::clone(&complaints));
                }
                (Box::new(tokio::io::join(stdout, stdin)), Some(spawned))
            }
            Transport::Local { socket } => {
                let stream =
                    UnixStream::connect(socket)
                        .await
                        .map_err(|source| ChannelError::Io {
                            host: host.clone(),
                            source,
                        })?;
                (Box::new(stream), None)
            }
        };
        RemoteChannel::shake_hands(FramedLink::new(wire), options, host, child, complaints).await
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
        let ToClient::Hello {
            protocol_version,
            server_version,
            capabilities,
        } = message
        else {
            return Err(ChannelError::Unexpected {
                host: host.to_owned(),
                wanted: "the server's Hello",
                received: format!("{message:?}"),
            });
        };
        if protocol_version != PROTOCOL_VERSION {
            return Err(ChannelError::ProtocolVersion {
                host: host.to_owned(),
                server: protocol_version,
            });
        }
        Ok(ServerHello {
            protocol_version,
            server_version,
            capabilities,
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
        let heard = Arc::new(Mutex::new(Instant::now()));
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
            let silent_since = *self.heard.lock().await;
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
            *self.heard.lock().await = Instant::now();
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
