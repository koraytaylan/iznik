//! The protocol test client: `iznik/1` over any duplex stream, the client the
//! macOS application will resemble minus the surface.
//!
//! It is written once, here, so that no test in the daemon plan or the client
//! plan hand-rolls a second protocol client — two clients would be two
//! readings of the same document, and the one that is wrong would be the one
//! nobody tested against.
//!
//! It keeps what it has been told rather than making its caller catch it: the
//! deltas in the order they arrived, and each pane's bytes concatenated in
//! order on its channel. A method that waits for one answer — a snapshot, a
//! command's result — records everything it passes over on the way.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::Path;
use std::time::Duration;

use iznik_link::compression::{ZstdStream, compressed};
use iznik_link::framed::{FramedLink, LinkError};
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{
    CommandOutcome, SessionCommand, decode_command_outcome, encode_session_command,
};
use iznik_protocol::delta::{Delta, decode_delta};
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence};
use iznik_protocol::message::{
    CHANNEL_CONTROL, MessageError, PROTOCOL_VERSION, ToClient, ToServer, decode_to_client,
    encode_to_server,
};
use iznik_protocol::model::{HostModel, decode_host_model};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;

/// How long a method that waits for one answer waits for it. Every test in
/// this workspace runs under a deadline; this is the one a caller does not
/// have to name.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);

/// What a stream must be for a client to speak over it.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin {}

impl<Stream: AsyncRead + AsyncWrite + Unpin> Duplex for Stream {}

/// Why a client could not do something.
#[derive(Debug)]
pub enum ClientError {
    /// The link failed.
    Link(LinkError),
    /// A message could not be encoded or decoded.
    Message(MessageError),
    /// The stream could not be opened, or compression could not be engaged.
    Io {
        /// What the operating system said.
        source: io::Error,
    },
    /// Nothing arrived in time.
    Deadline {
        /// How long it waited.
        waited: Duration,
    },
    /// The server closed the connection.
    Closed,
    /// The server said something else where one thing was expected.
    Unexpected {
        /// What was expected.
        wanted: &'static str,
        /// What arrived.
        received: String,
    },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Link(source) => write!(formatter, "the link failed: {source}"),
            ClientError::Message(source) => write!(formatter, "a message failed: {source}"),
            ClientError::Io { source } => write!(formatter, "the stream failed: {source}"),
            ClientError::Deadline { waited } => {
                write!(formatter, "nothing arrived in {waited:?}")
            }
            ClientError::Closed => write!(formatter, "the server closed the connection"),
            ClientError::Unexpected { wanted, received } => {
                write!(formatter, "expected {wanted}, received {received}")
            }
        }
    }
}

impl std::error::Error for ClientError {}

impl From<LinkError> for ClientError {
    fn from(source: LinkError) -> ClientError {
        ClientError::Link(source)
    }
}

impl From<MessageError> for ClientError {
    fn from(source: MessageError) -> ClientError {
        ClientError::Message(source)
    }
}

impl From<io::Error> for ClientError {
    fn from(source: io::Error) -> ClientError {
        ClientError::Io { source }
    }
}

/// What the server said in its half of the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerHello {
    /// The protocol version the server speaks.
    pub protocol_version: u16,
    /// The server's own version, for logs.
    pub server_version: String,
    /// What the server can do.
    pub capabilities: Capabilities,
}

/// One thing the server sent: a control message, or a pane's bytes with the
/// channel they came in on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
    /// A control message from channel 0.
    Control(ToClient),
    /// Raw pane output, never parsed.
    PaneBytes {
        /// The channel it came in on.
        channel: u8,
        /// The bytes.
        bytes: Vec<u8>,
    },
}

/// Where the client's frames go: plain until both ends advertise compression,
/// and a zstd stream over the same socket after.
#[derive(Debug)]
enum Link<Stream: Duplex> {
    /// Before the handshake, or when either end did not offer compression.
    Plain(FramedLink<Stream>),
    /// After a handshake in which both ends did.
    Compressed(FramedLink<ZstdStream<Stream>>),
}

/// A client speaking `iznik/1` over one duplex stream.
#[derive(Debug)]
pub struct TestClient<Stream: Duplex> {
    /// Where its frames go, taken out only while compression is engaged.
    link: Option<Link<Stream>>,
    /// The number the next command is sent under.
    next_command: u64,
    /// Which pane each channel carries, learned from `PaneChannel`.
    channels: BTreeMap<u8, PaneId>,
    /// Every pane's bytes, in the order they arrived.
    bytes: BTreeMap<PaneId, Vec<u8>>,
    /// Every delta received, in the order it arrived.
    deltas: Vec<(Generation, Delta)>,
    /// Frames taken off the link by a method waiting for one answer, which a
    /// later `next` yields before it reads again.
    kept: VecDeque<Received>,
    /// Whether it returns credit for every byte and acknowledges every detach.
    auto_credit: bool,
}

impl TestClient<UnixStream> {
    /// A client connected to a daemon's socket.
    ///
    /// # Errors
    ///
    /// [`ClientError::Io`] when the socket cannot be reached.
    pub async fn connect(socket: &Path) -> Result<TestClient<UnixStream>, ClientError> {
        Ok(TestClient::over(UnixStream::connect(socket).await?))
    }
}

impl<Stream: Duplex> TestClient<Stream> {
    /// A client over a stream that is already open.
    #[must_use]
    pub fn over(stream: Stream) -> TestClient<Stream> {
        TestClient {
            link: Some(Link::Plain(FramedLink::new(stream))),
            next_command: 0,
            channels: BTreeMap::new(),
            bytes: BTreeMap::new(),
            deltas: Vec::new(),
            kept: VecDeque::new(),
            auto_credit: false,
        }
    }

    /// Returns credit for every pane byte as it arrives and acknowledges every
    /// detach, which is what a client that keeps up does. Off by default, so a
    /// test that wants to see a window run out can.
    pub fn auto_credit(&mut self, returning: bool) {
        self.auto_credit = returning;
    }

    /// Everything received on a pane's channel since it was subscribed.
    #[must_use]
    pub fn bytes_of(&self, pane: PaneId) -> &[u8] {
        self.bytes.get(&pane).map_or(&[], Vec::as_slice)
    }

    /// Every delta received, in the order it arrived.
    #[must_use]
    pub fn deltas(&self) -> &[(Generation, Delta)] {
        &self.deltas
    }

    /// Sends one control message.
    ///
    /// # Errors
    ///
    /// [`ClientError::Message`] when it will not fit a frame, and
    /// [`ClientError::Link`] when the link fails.
    async fn tell(&mut self, message: &ToServer) -> Result<(), ClientError> {
        let payload = encode_to_server(message)?;
        match self.link.as_mut().ok_or(ClientError::Closed)? {
            Link::Plain(link) => link.send(CHANNEL_CONTROL, &payload).await?,
            Link::Compressed(link) => link.send(CHANNEL_CONTROL, &payload).await?,
        }
        Ok(())
    }

    /// The next frame off the link, as a channel and its bytes, waiting no
    /// longer than `deadline`. Cancel-safe underneath, so a deadline that
    /// elapses has taken no bytes.
    ///
    /// # Errors
    ///
    /// [`ClientError::Deadline`] when nothing arrives in time,
    /// [`ClientError::Closed`] at a clean end of stream, and
    /// [`ClientError::Link`] when the link fails.
    async fn frame(&mut self, deadline: Duration) -> Result<(u8, Vec<u8>), ClientError> {
        let link = self.link.as_mut().ok_or(ClientError::Closed)?;
        let reading = async {
            match link {
                Link::Plain(link) => link.next_frame().await,
                Link::Compressed(link) => link.next_frame().await,
            }
        };
        let Ok(frame) = tokio::time::timeout(deadline, reading).await else {
            return Err(ClientError::Deadline { waited: deadline });
        };
        let frame = frame?.ok_or(ClientError::Closed)?;
        Ok((frame.channel, frame.payload.to_vec()))
    }
}

/// Whether a capability set offers streaming compression.
fn offers_zstd(capabilities: Capabilities) -> bool {
    capabilities.bits() & Capabilities::ZSTD.bits() != 0
}

impl<Stream: Duplex> TestClient<Stream> {
    /// The handshake: says what this client is and can do, reads the server's
    /// reply, and engages compression when both halves offered it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unexpected`] when the first frame is not the server's
    /// `Hello`, [`ClientError::Io`] when compression cannot be engaged, and
    /// the link and deadline refusals of [`TestClient::next`].
    pub async fn hello(&mut self, capabilities: Capabilities) -> Result<ServerHello, ClientError> {
        self.tell(&ToServer::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_version: concat!("iznik-testkit ", env!("CARGO_PKG_VERSION")).to_owned(),
            capabilities,
        })
        .await?;
        let (channel, payload) = self.frame(DEFAULT_DEADLINE).await?;
        if channel != CHANNEL_CONTROL {
            return Err(ClientError::Unexpected {
                wanted: "the server's Hello on channel 0",
                received: format!("{} bytes on channel {channel}", payload.len()),
            });
        }
        let message = decode_to_client(&payload)?;
        let ToClient::Hello {
            protocol_version,
            server_version,
            capabilities: theirs,
        } = message
        else {
            return Err(ClientError::Unexpected {
                wanted: "the server's Hello",
                received: format!("{message:?}"),
            });
        };
        if offers_zstd(capabilities) && offers_zstd(theirs) {
            self.engage()?;
        }
        Ok(ServerHello {
            protocol_version,
            server_version,
            capabilities: theirs,
        })
    }

    /// Puts a zstd stream under the link, carrying over the bytes the plain
    /// one read past the server's `Hello` — those bytes are the start of the
    /// compressed stream, and a client that drops them cannot decode the
    /// first frame after the handshake.
    ///
    /// # Errors
    ///
    /// [`ClientError::Io`] when zstd cannot build a context around the
    /// dictionary, and [`ClientError::Closed`] when there is no link.
    fn engage(&mut self) -> Result<(), ClientError> {
        match self.link.take().ok_or(ClientError::Closed)? {
            Link::Plain(plain) => {
                let (stream, leftover) = plain.into_parts();
                self.link = Some(Link::Compressed(compressed(stream, leftover)?));
            }
            already @ Link::Compressed(_) => self.link = Some(already),
        }
        Ok(())
    }

    /// Asks for the complete host model without waiting for it, so a caller
    /// can see what arrives before it. [`TestClient::snapshot`] is this and
    /// the wait together.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn snapshot_request(&mut self) -> Result<(), ClientError> {
        self.tell(&ToServer::SnapshotRequest).await
    }

    /// The complete host model.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`], and [`ClientError::Message`] when the model
    /// will not decode.
    pub async fn snapshot(&mut self) -> Result<HostModel, ClientError> {
        self.snapshot_request().await?;
        let payload = self
            .expect(|message| match message {
                ToClient::Snapshot { payload, .. } => Some(payload.clone()),
                _other => None,
            })
            .await?;
        Ok(decode_host_model(&payload)?)
    }

    /// Sends a session command and waits for the result that answers it.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`], and [`ClientError::Message`] when the command
    /// will not encode or the outcome will not decode.
    pub async fn command(
        &mut self,
        command: SessionCommand,
    ) -> Result<CommandOutcome, ClientError> {
        let command_id = CommandId(self.next_command);
        self.next_command = self.next_command.saturating_add(1);
        let payload = encode_session_command(&command)?;
        self.tell(&ToServer::Command {
            command_id,
            payload,
        })
        .await?;
        let answer = self
            .expect(|message| match message {
                ToClient::CommandResult {
                    command_id: answered,
                    payload: outcome,
                } if *answered == command_id => Some(outcome.clone()),
                _other => None,
            })
            .await?;
        Ok(decode_command_outcome(&answer)?)
    }

    /// Begins delivery of a pane's output from wherever it now is.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn subscribe(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.tell(&ToServer::Subscribe { pane }).await
    }

    /// Ends delivery of a pane's output.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn unsubscribe(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.tell(&ToServer::Unsubscribe { pane }).await
    }

    /// Begins delivery from a byte position this client already holds.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn resume(&mut self, pane: PaneId, from: Sequence) -> Result<(), ClientError> {
        self.tell(&ToServer::Resume {
            pane,
            from_sequence: from,
        })
        .await
    }

    /// Asks for a pane's current screen as VT bytes.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn screen_request(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.tell(&ToServer::ScreenRequest { pane }).await
    }

    /// Sends keystrokes, paste, or this client's own query responses.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn input(&mut self, pane: PaneId, bytes: Vec<u8>) -> Result<(), ClientError> {
        self.tell(&ToServer::Input { pane, bytes }).await
    }

    /// Sets a pane's size.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn resize(
        &mut self,
        pane: PaneId,
        columns: u16,
        rows: u16,
    ) -> Result<(), ClientError> {
        self.tell(&ToServer::Resize {
            pane,
            columns,
            rows,
        })
        .await
    }

    /// Says which pane this client is looking at.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn focus(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.tell(&ToServer::Focus { pane }).await
    }

    /// Returns flow-control credit for a pane channel.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn credit(&mut self, channel: u8, bytes: u32) -> Result<(), ClientError> {
        self.tell(&ToServer::Credit { channel, bytes }).await
    }

    /// Says nothing of a detached pane is still in flight, so the channel
    /// number can be used again.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn channel_released(&mut self, channel: u8) -> Result<(), ClientError> {
        self.tell(&ToServer::ChannelReleased { channel }).await
    }

    /// Asks whether the server is there.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    pub async fn ping(&mut self) -> Result<(), ClientError> {
        self.tell(&ToServer::Ping).await
    }
}

impl<Stream: Duplex> TestClient<Stream> {
    /// The next thing the server sent, waiting no longer than `deadline`.
    ///
    /// Everything it yields it has already recorded: a delta in
    /// [`TestClient::deltas`], a pane's bytes in [`TestClient::bytes_of`], and
    /// the channel a `PaneChannel` named. Under [`TestClient::auto_credit`] it
    /// also returns credit for the bytes and acknowledges a detach.
    ///
    /// # Errors
    ///
    /// [`ClientError::Deadline`] when nothing arrives in time,
    /// [`ClientError::Closed`] when the server closes,
    /// [`ClientError::Link`] when the link fails, and
    /// [`ClientError::Message`] when a control message will not decode.
    pub async fn next(&mut self, deadline: Duration) -> Result<Received, ClientError> {
        if let Some(held) = self.kept.pop_front() {
            return Ok(held);
        }
        let (channel, payload) = self.frame(deadline).await?;
        self.record(channel, payload).await
    }

    /// Records one frame and says what it was.
    ///
    /// # Errors
    ///
    /// [`ClientError::Message`] when a control message or a delta will not
    /// decode, and the link's refusals when automatic credit cannot be sent.
    async fn record(&mut self, channel: u8, payload: Vec<u8>) -> Result<Received, ClientError> {
        if channel != CHANNEL_CONTROL {
            if let Some(pane) = self.channels.get(&channel).copied() {
                self.bytes
                    .entry(pane)
                    .or_default()
                    .extend_from_slice(&payload);
            }
            if self.auto_credit {
                let bytes = u32::try_from(payload.len()).unwrap_or(u32::MAX);
                self.credit(channel, bytes).await?;
            }
            return Ok(Received::PaneBytes {
                channel,
                bytes: payload,
            });
        }
        let message = decode_to_client(&payload)?;
        self.absorb(&message).await?;
        Ok(Received::Control(message))
    }

    /// Takes what a control message says about this client's own state.
    ///
    /// # Errors
    ///
    /// [`ClientError::Message`] when a delta will not decode, and the link's
    /// refusals when an acknowledgement cannot be sent.
    async fn absorb(&mut self, message: &ToClient) -> Result<(), ClientError> {
        match message {
            ToClient::PaneChannel { pane, channel, .. } => {
                let _carried = self.channels.insert(*channel, *pane);
            }
            ToClient::PaneDetached { channel, .. } => {
                let released = *channel;
                let _carried = self.channels.remove(&released);
                if self.auto_credit {
                    self.channel_released(released).await?;
                }
            }
            ToClient::Delta {
                generation,
                payload,
            } => self.deltas.push((*generation, decode_delta(payload)?)),
            _other => {}
        }
        Ok(())
    }

    /// The first control message `matching` claims, taken from what is already
    /// held and then from the link. Everything passed over stays in order for
    /// [`TestClient::next`] to yield.
    ///
    /// # Errors
    ///
    /// As [`TestClient::next`].
    async fn expect<Found>(
        &mut self,
        matching: impl Fn(&ToClient) -> Option<Found>,
    ) -> Result<Found, ClientError> {
        let mut passed = VecDeque::with_capacity(self.kept.len());
        let mut found = None;
        while let Some(held) = self.kept.pop_front() {
            if found.is_none()
                && let Received::Control(message) = &held
            {
                found = matching(message);
                if found.is_some() {
                    continue;
                }
            }
            passed.push_back(held);
        }
        self.kept = passed;
        if let Some(found) = found {
            return Ok(found);
        }
        let started = std::time::Instant::now();
        loop {
            let waited = started.elapsed();
            let Some(left) = DEFAULT_DEADLINE.checked_sub(waited) else {
                return Err(ClientError::Deadline { waited });
            };
            let (channel, payload) = self.frame(left).await?;
            let received = self.record(channel, payload).await?;
            if let Received::Control(message) = &received
                && let Some(claimed) = matching(message)
            {
                return Ok(claimed);
            }
            self.kept.push_back(received);
        }
    }
}
