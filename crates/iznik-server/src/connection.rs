//! One accepted stream: the handshake, the dispatch of every control message,
//! and the single writer that owns the order of frames on the wire.
//!
//! It is written over any duplex stream rather than over a socket, so it is
//! proven against a socket pair with no daemon, no socket file and no process
//! — and so the daemon has nothing to add to it but an accept loop.
//!
//! Three tasks and two channels. The reader decodes frames and hands each
//! request on; one task owns the multiplexer, dispatches every request and
//! drives the pump; one owns the link's writer half and does nothing else, so
//! two tasks never race for the socket and the order of frames is one task's
//! decision. Both channels are bounded: a client that stops reading stops the
//! server writing, which is what a credit window is for and what keeps the
//! multiplexer's promise to buffer nothing.
//!
//! A client that disconnects leaves nothing behind. Dropping the multiplexer
//! drops every `Subscription` — so the panes it watched answer a program's
//! terminal queries themselves again — ends every watcher task and frees every
//! channel; nothing about the sessions changes.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use iznik_link::compression::compressed;
use iznik_link::framed::{FrameWriter, FramedLink, LinkError};
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{BuildDigest, ClientIdentity, DaemonInstance, PaneId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, ErrorCode, MessageError, PROTOCOL_VERSION, ToClient, ToServer,
    decode_to_server, encode_to_client,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

use crate::multiplexer::channel::{MultiplexerError, SinkError};
use crate::multiplexer::{FrameSink, Multiplexer};
use crate::pane::PaneError;
use crate::pty::streams::{InputError, InputRoom, Offer};
use crate::resume::StartRequest;
use crate::session::commands;
use crate::session::registry::Registry;
use crate::upload::UploadState;

/// This server's own version, which the client keeps only for its logs.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The adopt bit on a host that can keep its sessions, and none on Windows.
const fn adopt_capability() -> u32 {
    #[cfg(unix)]
    {
        Capabilities::ADOPT.bits()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// What this server can do.
const CAPABILITIES: Capabilities = Capabilities::from_bits(
    Capabilities::ZSTD.bits()
        | Capabilities::RESUME.bits()
        | Capabilities::REORDER_SESSIONS.bits()
        | adopt_capability()
        | Capabilities::INSTANCE.bits()
        | Capabilities::ANSWERED.bits()
        | Capabilities::IDENTIFY.bits()
        | Capabilities::BUILD.bits()
        | Capabilities::UPLOAD.bits(),
);

/// How many frames may be waiting for the writer. Small on purpose: it is not
/// a queue, it is the hand-off, and a client that stops reading must stop the
/// server producing rather than fill its memory.
const FRAME_QUEUE: usize = 8;

/// How many decoded requests may be waiting for the dispatcher, for the same
/// reason.
const REQUEST_QUEUE: usize = 8;

/// How long a peer that has connected has to say `Hello`. A client says it at
/// once; a peer that connects and says nothing — a half-open socket, a stuck
/// process — would otherwise hold a connection task and its descriptor for as
/// long as the daemon runs.
pub const GREETING_DEADLINE: Duration = Duration::from_secs(10);

/// The timing a connection runs under, so a test can shorten it.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionOptions {
    /// How long the peer has to send its `Hello`.
    pub greeting_deadline: Duration,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        ConnectionOptions {
            greeting_deadline: GREETING_DEADLINE,
        }
    }
}

/// Why a connection ended.
#[derive(Debug)]
pub enum ConnectionError {
    /// The link failed or the peer went away mid-frame.
    Link(LinkError),
    /// The peer's first frame was not a `Hello`, or a later frame did not
    /// decode. A peer that speaks garbage is disconnected; a well-formed
    /// request that is wrong is refused and the connection stays open.
    Garbage {
        /// What was wrong with it.
        detail: String,
    },
    /// The peer connected and did not say `Hello` within the greeting
    /// deadline.
    Silent,
    /// The peer speaks a protocol version this server does not.
    ProtocolVersion {
        /// The version it said.
        spoken: u16,
    },
    /// Compression could not be engaged after both ends asked for it.
    Compression {
        /// What zstd said.
        detail: String,
    },
    /// The multiplexer refused something that is not the client's to fix.
    Multiplexer(MultiplexerError),
    /// One of the connection's own tasks ended abruptly, which is a bug here
    /// and not a client's doing.
    Task {
        /// What the runtime said.
        detail: String,
    },
}

impl core::fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ConnectionError::Link(source) => write!(formatter, "the link: {source}"),
            ConnectionError::Garbage { detail } => {
                write!(formatter, "the client spoke garbage: {detail}")
            }
            ConnectionError::Silent => {
                write!(formatter, "the client did not say hello in time")
            }
            ConnectionError::ProtocolVersion { spoken } => write!(
                formatter,
                "the client speaks protocol {spoken}, this server speaks {PROTOCOL_VERSION}"
            ),
            ConnectionError::Compression { detail } => {
                write!(formatter, "compression could not be engaged: {detail}")
            }
            ConnectionError::Multiplexer(source) => write!(formatter, "the multiplexer: {source}"),
            ConnectionError::Task { detail } => {
                write!(formatter, "a connection task ended abruptly: {detail}")
            }
        }
    }
}

impl core::error::Error for ConnectionError {}

impl From<LinkError> for ConnectionError {
    fn from(source: LinkError) -> ConnectionError {
        ConnectionError::Link(source)
    }
}

impl From<MessageError> for ConnectionError {
    fn from(source: MessageError) -> ConnectionError {
        ConnectionError::Garbage {
            detail: source.to_string(),
        }
    }
}

impl From<MultiplexerError> for ConnectionError {
    fn from(source: MultiplexerError) -> ConnectionError {
        ConnectionError::Multiplexer(source)
    }
}

/// One frame on its way out.
#[derive(Debug)]
struct Outgoing {
    /// The channel it belongs to.
    channel: u8,
    /// Its bytes.
    payload: Vec<u8>,
}

/// The multiplexer's sink: a hand-off to the one task that writes.
#[derive(Debug)]
struct Handoff {
    /// Where frames go.
    frames: mpsc::Sender<Outgoing>,
}

impl FrameSink for Handoff {
    fn send(
        &mut self,
        channel: u8,
        payload: Vec<u8>,
    ) -> impl Future<Output = Result<(), SinkError>> + Send {
        let outgoing = Outgoing { channel, payload };
        let frames = self.frames.clone();
        async move {
            frames
                .send(outgoing)
                .await
                .map_err(|_gone| SinkError::Closed)
        }
    }
}

/// The one task that writes: everything the connection sends passes through
/// it, in the order it was handed over.
///
/// # Errors
///
/// [`ConnectionError::Link`] when the link fails.
async fn write_frames<Stream>(
    mut writer: FrameWriter<Stream>,
    mut frames: mpsc::Receiver<Outgoing>,
) -> Result<(), ConnectionError>
where
    Stream: AsyncWrite + Unpin,
{
    while let Some(outgoing) = frames.recv().await {
        writer.send(outgoing.channel, &outgoing.payload).await?;
    }
    Ok(())
}

/// What the client said in the first frame, and the link to carry on with.
struct Greeted<Stream: AsyncRead + AsyncWrite + Unpin> {
    /// The link, plain still, ready to be split or wrapped.
    link: FramedLink<Stream>,
    /// What the client can do.
    capabilities: Capabilities,
}

/// Reads the client's `Hello`, refuses a version this server does not speak,
/// and answers with the server's own.
///
/// # Errors
///
/// [`ConnectionError::Garbage`] when the first frame is not a `Hello`,
/// [`ConnectionError::ProtocolVersion`] when the versions differ — after the
/// client has been told, so it can say why it failed — and
/// [`ConnectionError::Link`] when the link fails, and
/// [`ConnectionError::Silent`] when nothing arrives within `deadline`.
async fn greet<Stream>(
    mut link: FramedLink<Stream>,
    deadline: Duration,
    daemon: (DaemonInstance, Option<BuildDigest>),
) -> Result<Option<Greeted<Stream>>, ConnectionError>
where
    Stream: AsyncRead + AsyncWrite + Unpin,
{
    // A peer that connects and closes without speaking has said nothing wrong:
    // that is what a readiness probe looks like from here.
    let Some(first) = tokio::time::timeout(deadline, link.next_frame())
        .await
        .map_err(|_late| ConnectionError::Silent)??
    else {
        return Ok(None);
    };
    if first.channel != CHANNEL_CONTROL {
        return Err(ConnectionError::Garbage {
            detail: format!("the first frame arrived on channel {}", first.channel),
        });
    }
    let greeting = decode_to_server(first.payload)?;
    let ToServer::Hello {
        protocol_version,
        capabilities,
        ..
    } = greeting
    else {
        return Err(ConnectionError::Garbage {
            detail: format!("the first frame was {greeting:?}, not a Hello"),
        });
    };
    if protocol_version != PROTOCOL_VERSION {
        let refusal = ToClient::Error {
            code: ErrorCode::ProtocolVersion,
            message: format!("this server speaks protocol {PROTOCOL_VERSION}"),
        };
        link.send(CHANNEL_CONTROL, &encode_to_client(&refusal)?)
            .await?;
        return Err(ConnectionError::ProtocolVersion {
            spoken: protocol_version,
        });
    }
    // A field a capability appends goes only to a client that said it can
    // read it: an older one refuses a message longer than the fields it
    // knows, and would never connect. Such a client is told the capability is
    // absent too, so what it is sent and what it was told agree.
    // The build goes after the instance and only after it, and only from a
    // daemon that knows it: a reply claims the bit exactly when it carries it.
    let (instance, known) = daemon;
    let told_instance = capabilities.contains(Capabilities::INSTANCE);
    let build = known.filter(|_known| told_instance && capabilities.contains(Capabilities::BUILD));
    let mut unreadable = Capabilities::APPENDED.bits() & !capabilities.bits();
    if build.is_none() {
        unreadable |= Capabilities::BUILD.bits();
    }
    let reply = ToClient::Hello {
        protocol_version: PROTOCOL_VERSION,
        server_version: SERVER_VERSION.to_owned(),
        capabilities: Capabilities::from_bits(CAPABILITIES.bits() & !unreadable),
        instance: told_instance.then_some(instance),
        build,
    };
    link.send(CHANNEL_CONTROL, &encode_to_client(&reply)?)
        .await?;
    Ok(Some(Greeted { link, capabilities }))
}

/// Whether a capability set offers streaming compression.
fn offers_zstd(capabilities: Capabilities) -> bool {
    capabilities.bits() & Capabilities::ZSTD.bits() != 0
}

/// Serves one client until it goes: the handshake, then the loop.
///
/// A `Hello` carrying a version this server does not speak is answered and the
/// connection closes; a frame that does not decode closes it without an
/// answer, because a peer that speaks garbage is not making a request. Every
/// well-formed request that is wrong is refused with an `Error` and the
/// connection stays open.
///
/// # Errors
///
/// [`ConnectionError`] for every way a connection ends other than the client
/// closing it cleanly, which is `Ok`.
pub async fn serve<Stream>(
    stream: Stream,
    registry: Arc<RwLock<Registry>>,
) -> Result<(), ConnectionError>
where
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_with_options(stream, registry, ConnectionOptions::default()).await
}

/// [`serve`] under timing of the caller's choosing.
///
/// # Errors
///
/// As [`serve`].
pub async fn serve_with_options<Stream>(
    stream: Stream,
    registry: Arc<RwLock<Registry>>,
    options: ConnectionOptions,
) -> Result<(), ConnectionError>
where
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let daemon = {
        let held = registry.read().await;
        (held.instance(), held.build())
    };
    let Some(greeted) = greet(FramedLink::new(stream), options.greeting_deadline, daemon).await?
    else {
        return Ok(());
    };
    let answering = greeted.capabilities.contains(Capabilities::ANSWERED);
    if offers_zstd(greeted.capabilities) && offers_zstd(CAPABILITIES) {
        let (plain, leftover) = greeted.link.into_parts();
        let link = compressed(plain, leftover).map_err(|error| ConnectionError::Compression {
            detail: error.to_string(),
        })?;
        return run(link, registry, answering).await;
    }
    run(greeted.link, registry, answering).await
}

/// The three tasks, running until the client goes.
///
/// # Errors
///
/// [`ConnectionError`] as [`serve`] documents it.
async fn run<Stream>(
    link: FramedLink<Stream>,
    registry: Arc<RwLock<Registry>>,
    answering: bool,
) -> Result<(), ConnectionError>
where
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, writer) = link.split();
    let (frames, queued) = mpsc::channel(FRAME_QUEUE);
    let (requests, asked) = mpsc::channel(REQUEST_QUEUE);
    let (deltas, _attached) = {
        let held = registry.read().await;
        (held.deltas(), held.attach_client())
    };
    let mut multiplexer = Multiplexer::new(Arc::clone(&registry), Handoff { frames }, deltas);
    if answering {
        multiplexer.announce_answered();
    }
    let writing: JoinHandle<Result<(), ConnectionError>> =
        tokio::spawn(write_frames(writer, queued));
    let pumping: JoinHandle<Result<(), ConnectionError>> =
        tokio::spawn(dispatch(multiplexer, registry, asked));

    // Reading is this task's own work: when it ends, dropping the request
    // channel ends the dispatcher, which drops the multiplexer — every
    // subscription with it — and that ends the writer.
    let mut ended = Ok(());
    loop {
        let read = tokio::select! {
            biased;
            // The dispatcher gave up: nothing will answer this client again,
            // so the connection ends rather than holding a socket open on a
            // reply that is never coming.
            () = requests.closed() => break,
            read = reader.next_frame() => read,
        };
        let frame = match read {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(refused) => {
                ended = Err(ConnectionError::Link(refused));
                break;
            }
        };
        if frame.channel != CHANNEL_CONTROL {
            ended = Err(ConnectionError::Garbage {
                detail: format!("a client sent on channel {}", frame.channel),
            });
            break;
        }
        let request = match decode_to_server(frame.payload) {
            Ok(request) => request,
            Err(refused) => {
                ended = Err(ConnectionError::from(refused));
                break;
            }
        };
        if requests.send(request).await.is_err() {
            break;
        }
    }
    drop(requests);
    let pumped = pumping.await;
    let wrote = writing.await;
    ended?;
    match (pumped, wrote) {
        (Ok(pumped), Ok(wrote)) => pumped.and(wrote),
        (Err(joined), _) | (_, Err(joined)) => Err(ConnectionError::Task {
            detail: joined.to_string(),
        }),
    }
}

/// How many requests may wait behind keystrokes whose pane has no room for
/// them before the dispatcher stops taking more from the reader. Credit and
/// pings are still answered while they wait; past this, the reader stops too,
/// and the client's own writes back up behind it — which is what holding a
/// paste rather than dropping part of it costs. Each may be a whole frame of
/// keystrokes, so this bounds what one connection holds at a few megabytes.
const HELD_REQUESTS: usize = 8;

/// Keystrokes their pane had no room for, and the way to wait for some.
struct Blocked {
    /// The pane.
    pane: PaneId,
    /// The keystrokes, untouched.
    bytes: Vec<u8>,
    /// Resolves once the pane's input has taken some of what it held.
    room: InputRoom,
}

/// The requests this connection has taken and not yet answered, in order.
#[derive(Default)]
struct Held {
    /// Waiting behind `blocked`, or not yet reached.
    requests: VecDeque<ToServer>,
    /// Keystrokes waiting for their pane to make room, ahead of everything
    /// in `requests`.
    blocked: Option<Blocked>,
    /// Who the client said it is, once it has: what its commands are
    /// remembered under.
    client: Option<ClientIdentity>,
    /// Files this connection is still receiving. Dropping the connection
    /// deletes whatever had not been finished.
    upload_state: UploadState,
}

impl Held {
    /// Whether another request may be taken from the reader.
    fn has_room(&self) -> bool {
        self.requests.len() < HELD_REQUESTS
    }

    /// Resolves once the pane blocking the queue may have room; never, when
    /// nothing is blocked.
    async fn room(&mut self) {
        match &mut self.blocked {
            Some(blocked) => blocked.room.wait().await,
            None => std::future::pending().await,
        }
    }
}

/// What woke the dispatcher.
enum Woken {
    /// The client asked for something, or its reader ended.
    Asked(Option<ToServer>),
    /// A pane or the model has something to say.
    Ready,
}

/// The task that owns the multiplexer: it answers every request and drives the
/// pump, so the two never run at once and the multiplexer needs no lock.
///
/// # Errors
///
/// [`ConnectionError`] for a refusal the client cannot act on.
async fn dispatch(
    mut multiplexer: Multiplexer<Handoff>,
    registry: Arc<RwLock<Registry>>,
    mut asked: mpsc::Receiver<ToServer>,
) -> Result<(), ConnectionError> {
    let signal = registry.read().await.signal();
    // Whether the last round still had something to send. While it has, the
    // loop takes only the requests already waiting: pumping to exhaustion
    // instead would let a flooding pane whose client keeps returning credit
    // starve every request, and a keystroke would wait for the flood to end.
    // One round per turn is what the scheduler's own promise means — a
    // keystroke waits behind at most one frame per active pane — and every
    // request already waiting is answered each turn, credits and pings last,
    // so a keystroke never queues behind a line of credits that arrived with
    // it.
    let mut sending = false;
    let mut held = Held::default();
    loop {
        let first = if sending {
            None
        } else {
            let taking = held.has_room();
            let woken = tokio::select! {
                biased;
                received = asked.recv(), if taking => Woken::Asked(received),
                () = held.room() => Woken::Ready,
                () = multiplexer.ready() => Woken::Ready,
                () = signal.notified() => Woken::Ready,
            };
            match woken {
                Woken::Asked(None) => return Ok(()),
                Woken::Asked(Some(request)) => Some(request),
                Woken::Ready => None,
            }
        };
        let (commuting, gone) = take_waiting(&mut asked, first, &mut held);
        answer_held(&mut multiplexer, &registry, &mut held).await?;
        for request in commuting {
            // Nothing that commutes is keystrokes, so nothing here is held.
            let client = held.client;
            let _held = answer(
                &mut multiplexer,
                &registry,
                client,
                &mut held.upload_state,
                request,
            )
            .await?;
        }
        if gone {
            return Ok(());
        }
        // Nothing else turns what the panes have reported into deltas, and a
        // client's own `Resize` must reach it as a `PaneResized` like any
        // other client's.
        registry.write().await.ingest();
        sending = multiplexer.pump().await?;
    }
}

/// Takes `first`, if any, and every request already waiting behind it: those
/// which [`commutes`] are given back to be answered at once, after the rest,
/// and the rest are queued in `held` in the order they came, until it is
/// full. Says too whether the reader has gone, which is said only once every
/// request it sent has been taken.
fn take_waiting(
    asked: &mut mpsc::Receiver<ToServer>,
    first: Option<ToServer>,
    held: &mut Held,
) -> (Vec<ToServer>, bool) {
    let mut commuting = Vec::new();
    let mut next = first;
    let gone = loop {
        if let Some(request) = next.take() {
            if commutes(&request) {
                commuting.push(request);
            } else {
                held.requests.push_back(request);
            }
        }
        if !held.has_room() {
            break false;
        }
        match asked.try_recv() {
            Ok(request) => next = Some(request),
            Err(mpsc::error::TryRecvError::Empty) => break false,
            Err(mpsc::error::TryRecvError::Disconnected) => break true,
        }
    };
    (commuting, gone)
}

/// Answers the requests `held` queues, in order, until one is keystrokes whose
/// pane has no room for them: those wait, and everything behind them with
/// them, so a paste reaches its pane whole and nothing overtakes it.
///
/// # Errors
///
/// As [`answer`].
async fn answer_held(
    multiplexer: &mut Multiplexer<Handoff>,
    registry: &Arc<RwLock<Registry>>,
    held: &mut Held,
) -> Result<(), ConnectionError> {
    if let Some(Blocked { pane, bytes, .. }) = held.blocked.take() {
        held.blocked = offer(multiplexer, registry, pane, bytes).await?;
        if held.blocked.is_some() {
            return Ok(());
        }
    }
    while let Some(request) = held.requests.pop_front() {
        // In order with the commands it names: one sent before it is not
        // remembered, and every one after it is.
        if let ToServer::Identify { client } = request {
            held.client = Some(client);
            continue;
        }
        let client = held.client;
        held.blocked = answer(
            multiplexer,
            registry,
            client,
            &mut held.upload_state,
            request,
        )
        .await?;
        if held.blocked.is_some() {
            return Ok(());
        }
    }
    Ok(())
}

/// Whether a request's answer is the same whenever in a turn it is given:
/// credit, a ping, a channel released. Anything else changes a pane or what
/// this client watches — a keystroke must not overtake the resize it was
/// typed after, nor reach a pane closed before it — and keeps its place.
fn commutes(request: &ToServer) -> bool {
    matches!(
        request,
        ToServer::Credit { .. } | ToServer::Ping | ToServer::ChannelReleased { .. }
    )
}

/// Answers a refusal the client can act on with an `Error` and leaves the
/// connection open; passes on one it cannot.
///
/// # Errors
///
/// [`ConnectionError::Multiplexer`] for a refusal that is not the client's to
/// fix, and the link's refusals when the answer cannot be sent.
async fn refused(
    multiplexer: &mut Multiplexer<Handoff>,
    outcome: Result<(), MultiplexerError>,
) -> Result<(), ConnectionError> {
    let Err(refusal) = outcome else {
        return Ok(());
    };
    let code = match &refusal {
        MultiplexerError::ChannelsExhausted => ErrorCode::ChannelsExhausted,
        MultiplexerError::UnknownPane { .. } => ErrorCode::UnknownPane,
        MultiplexerError::NotSubscribed { .. }
        | MultiplexerError::NotReleased { .. }
        | MultiplexerError::UnknownChannel { .. } => ErrorCode::NotSubscribed,
        MultiplexerError::Encoding { .. }
        | MultiplexerError::Pane { .. }
        | MultiplexerError::Sink(_) => return Err(ConnectionError::Multiplexer(refusal)),
    };
    let message = refusal.to_string();
    multiplexer
        .reply(&ToClient::Error { code, message })
        .await?;
    Ok(())
}

/// Answers that the host holds no such pane.
///
/// # Errors
///
/// The link's refusals when the answer cannot be sent.
async fn no_such_pane(
    multiplexer: &mut Multiplexer<Handoff>,
    pane: PaneId,
) -> Result<(), ConnectionError> {
    let message = format!("the host holds no pane {}", pane.0);
    multiplexer
        .reply(&ToClient::Error {
            code: ErrorCode::UnknownPane,
            message,
        })
        .await
        .map_err(ConnectionError::from)
}

/// Answers a pane's refusal of a request.
///
/// # Errors
///
/// The link's refusals when the answer cannot be sent.
async fn pane_refused(
    multiplexer: &mut Multiplexer<Handoff>,
    pane: PaneId,
    refusal: &PaneError,
) -> Result<(), ConnectionError> {
    // Five codes exist and none of them says "this pane's terminal failed",
    // so the nearest is used and the message carries the truth. What exists is
    // the model's to say: a client learns a pane has gone from `PaneRemoved`,
    // never from a refusal's code.
    tracing::warn!(pane = pane.0, %refusal, "a pane refused a request");
    let code = match refusal {
        PaneError::Input(InputError::Backlog { .. }) => ErrorCode::InputBacklog,
        _other => ErrorCode::UnknownPane,
    };
    let message = format!("pane {}: {refusal}", pane.0);
    multiplexer
        .reply(&ToClient::Error { code, message })
        .await
        .map_err(ConnectionError::from)
}

/// Gives keystrokes to their pane, or hands them back to wait when the pane's
/// input has no room for them yet.
///
/// # Errors
///
/// The link's refusals when an answer cannot be sent.
async fn offer(
    multiplexer: &mut Multiplexer<Handoff>,
    registry: &Arc<RwLock<Registry>>,
    pane: PaneId,
    bytes: Vec<u8>,
) -> Result<Option<Blocked>, ConnectionError> {
    let held = registry.read().await.pane(pane).cloned();
    let Some(held) = held else {
        no_such_pane(multiplexer, pane).await?;
        return Ok(None);
    };
    match held.offer_input(bytes) {
        Ok(Offer::Taken) => Ok(None),
        Ok(Offer::Full { bytes, room }) => Ok(Some(Blocked { pane, bytes, room })),
        Err(refusal) => {
            pane_refused(multiplexer, pane, &refusal).await?;
            Ok(None)
        }
    }
}

/// Resizes a pane.
///
/// # Errors
///
/// The link's refusals when an answer cannot be sent.
async fn resize(
    multiplexer: &mut Multiplexer<Handoff>,
    registry: &Arc<RwLock<Registry>>,
    pane: PaneId,
    columns: u16,
    rows: u16,
) -> Result<(), ConnectionError> {
    let held = registry.read().await.pane(pane).cloned();
    let Some(held) = held else {
        return no_such_pane(multiplexer, pane).await;
    };
    match held.resize(columns, rows) {
        Ok(()) => Ok(()),
        Err(refusal) => pane_refused(multiplexer, pane, &refusal).await,
    }
}

/// The requests the multiplexer answers: what this client watches, where each
/// subscription starts, how much it may be sent, and which pane it is looking
/// at. Each refusal is the client's to act on or it is not, and `refused`
/// decides which.
///
/// # Errors
///
/// As [`refused`].
async fn watch(
    multiplexer: &mut Multiplexer<Handoff>,
    request: ToServer,
) -> Result<(), ConnectionError> {
    let outcome = match request {
        ToServer::Subscribe { pane } => {
            multiplexer
                .subscribe(StartRequest::Subscribe { pane })
                .await
        }
        ToServer::Resume {
            pane,
            from_sequence,
        } => {
            multiplexer
                .subscribe(StartRequest::Resume {
                    pane,
                    from: from_sequence,
                })
                .await
        }
        ToServer::ScreenRequest { pane } => {
            multiplexer
                .subscribe(StartRequest::ScreenRequest { pane })
                .await
        }
        ToServer::Unsubscribe { pane } => multiplexer.unsubscribe(pane).await,
        ToServer::Credit { channel, bytes } => multiplexer.credit(channel, bytes),
        ToServer::ChannelReleased { channel } => multiplexer.channel_released(channel),
        ToServer::Focus { pane } => multiplexer.focus(pane).await,
        _other => Ok(()),
    };
    refused(multiplexer, outcome).await
}

/// Answers one request, or — for keystrokes whose pane has no room for them
/// yet — hands them back to wait.
///
/// # Errors
///
/// [`ConnectionError::Garbage`] for a second `Hello`, which is a peer out of
/// step rather than one making a request; [`ConnectionError::Multiplexer`] for
/// a refusal the client cannot act on; and the link's refusals.
async fn answer(
    multiplexer: &mut Multiplexer<Handoff>,
    registry: &Arc<RwLock<Registry>>,
    client: Option<ClientIdentity>,
    upload_state: &mut UploadState,
    request: ToServer,
) -> Result<Option<Blocked>, ConnectionError> {
    match request {
        ToServer::Input { pane, bytes } => offer(multiplexer, registry, pane, bytes).await,
        other => answer_other(multiplexer, registry, client, upload_state, other)
            .await
            .map(|()| None),
    }
}

/// Answers one request that is not keystrokes.
///
/// # Errors
///
/// As [`answer`].
async fn answer_other(
    multiplexer: &mut Multiplexer<Handoff>,
    registry: &Arc<RwLock<Registry>>,
    client: Option<ClientIdentity>,
    upload_state: &mut UploadState,
    request: ToServer,
) -> Result<(), ConnectionError> {
    match request {
        ToServer::Hello { .. } => Err(ConnectionError::Garbage {
            detail: "a second Hello: the handshake happens once".to_owned(),
        }),
        ToServer::SnapshotRequest => Ok(multiplexer.send_snapshot().await?),
        ToServer::Command {
            command_id,
            payload,
        } => {
            // A payload that will not decode is refused, not fatal: ending the
            // connection over it would take every pane on the link with it,
            // and those panes are somebody's running sessions.
            let answered = commands::answer(registry, client, command_id, &payload).await?;
            multiplexer
                .reply(&ToClient::CommandResult {
                    command_id,
                    payload: answered,
                })
                .await?;
            Ok(())
        }
        ToServer::Ping => Ok(multiplexer.reply(&ToClient::Pong).await?),
        ToServer::Upload(upload) => {
            Ok(crate::upload::accept(multiplexer, registry, upload_state, upload).await?)
        }
        ToServer::Resize {
            pane,
            columns,
            rows,
        } => resize(multiplexer, registry, pane, columns, rows).await,
        watched => watch(multiplexer, watched).await,
    }
}
