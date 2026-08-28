//! Any number of clients, each with its own multiplexer and its own focus, all
//! seeing the same model — and a client that disconnects leaving no
//! subscription, no channel and no change to any session behind.
//!
//! The loop is written over any duplex stream, so every case here runs over a
//! socket pair: no daemon, no socket file, no process. The panes run `sh`.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, Created, SessionCommand};
use iznik_protocol::delta::{Delta, decode_delta};
use iznik_protocol::frame::MAXIMUM_PAYLOAD_LENGTH;
use iznik_protocol::identity::{Generation, PaneId, Sequence};
use iznik_protocol::message::{
    CHANNEL_CONTROL, ErrorCode, PROTOCOL_VERSION, ToClient, ToServer, decode_to_client,
    encode_to_server,
};
use iznik_server::connection::serve;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::pty::streams::MAXIMUM_PENDING_INPUT_BYTES;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::client::{ClientError, Received, TestClient};
use tokio::net::UnixStream;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// The size every pane in these cases is created at.
const COLUMNS: u16 = 80;
/// The height they are created at.
const ROWS: u16 = 24;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_mins(1);
/// The deadline a case gives something that should already be on its way.
const PROMPT: Duration = Duration::from_secs(2);
/// How long a case waits between looks at a shell.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How many looks it takes before it gives up on one.
const POLL_ATTEMPTS: usize = 400;

/// The bytes an `Input` message spends before its payload: the discriminant,
/// the pane id and the payload's length.
const INPUT_OVERHEAD: u32 = 13;

/// How much of a pane's flood the one-writer case waits for before it starts,
/// and how much must have reached the client by the time it ends.
const FLOODED_BYTES: usize = 128 * 1024;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A registry and the connections serving it.
struct Host {
    /// The host model and the panes behind it.
    registry: Arc<RwLock<Registry>>,
    /// One task per accepted stream, so a case can say how a connection ended.
    serving: Vec<JoinHandle<Result<(), iznik_server::connection::ConnectionError>>>,
}

impl Host {
    /// A host with no sessions and nobody connected.
    ///
    /// # Errors
    ///
    /// When the mirror thread will not start.
    fn new() -> Result<Host, Failed> {
        let registry = Registry::new(
            RegistryDefaults {
                program: Program::Command {
                    path: "sh".into(),
                    arguments: Vec::new(),
                },
                terminfo_directory: None,
            },
            Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES))),
            MirrorThread::start()?,
        );
        Ok(Host {
            registry: Arc::new(RwLock::new(registry)),
            serving: Vec::new(),
        })
    }

    /// A client on a socket pair, with a connection serving the other end. The
    /// handshake is not run: a case that wants one runs it.
    ///
    /// # Errors
    ///
    /// When the pair cannot be made.
    fn connect(&mut self) -> Result<TestClient<UnixStream>, Failed> {
        let (near, far) = UnixStream::pair()?;
        self.serving
            .push(tokio::spawn(serve(far, Arc::clone(&self.registry))));
        Ok(TestClient::over(near))
    }

    /// A client that has shaken hands, offering `capabilities`.
    ///
    /// # Errors
    ///
    /// When the pair cannot be made or the handshake fails.
    async fn attach(
        &mut self,
        capabilities: Capabilities,
    ) -> Result<TestClient<UnixStream>, Failed> {
        let mut client = self.connect()?;
        let greeting = client.hello(capabilities).await?;
        if greeting.protocol_version != PROTOCOL_VERSION {
            return Err("the server speaks another protocol".into());
        }
        Ok(client)
    }

    /// Every pane the host holds, in model order.
    async fn panes(&self) -> Vec<PaneId> {
        self.registry
            .read()
            .await
            .snapshot()
            .sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .map(|pane| pane.id)
            .collect()
    }

    /// Everything a pane has produced.
    ///
    /// # Errors
    ///
    /// When the host holds no such pane.
    async fn history(&self, pane: PaneId) -> Result<Vec<u8>, Failed> {
        let host = self.registry.read().await;
        let held = host.pane(pane).ok_or("the host holds no such pane")?;
        Ok(held.read_history(Sequence(0))?)
    }

    /// Waits until a pane has produced at least `wanted` bytes.
    ///
    /// # Errors
    ///
    /// When it has not after [`POLL_ATTEMPTS`] looks.
    async fn produced(&self, pane: PaneId, wanted: u64) -> Result<(), Failed> {
        for _attempt in 0..POLL_ATTEMPTS {
            let newest = self
                .registry
                .read()
                .await
                .pane(pane)
                .map_or(Sequence(0), |held| held.state().newest);
            if newest.0 >= wanted {
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Err(format!("pane {} never produced {wanted} bytes", pane.0).into())
    }
}

/// Runs a case under the deadline.
///
/// # Errors
///
/// Whatever the case reports, and when it does not finish in [`DEADLINE`].
async fn bounded<Case: Future<Output = Result<(), Failed>>>(case: Case) -> Result<(), Failed> {
    tokio::time::timeout(DEADLINE, case).await?
}

/// Creates a session and says which pane it made.
///
/// # Errors
///
/// When the command is refused or answered with something else.
async fn make_session(client: &mut TestClient<UnixStream>) -> Result<PaneId, Failed> {
    let outcome = client
        .command(SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        })
        .await?;
    let CommandOutcome::Applied { created, .. } = outcome else {
        return Err(format!("the session was refused: {outcome:?}").into());
    };
    let Created::Session(_session) = created else {
        return Err(format!("it created {created:?}").into());
    };
    Ok(PaneId(0))
}

/// Waits for the next delta of a kind, and says the generation it produced.
///
/// # Errors
///
/// When none arrives within [`PROMPT`].
async fn delta_of(
    client: &mut TestClient<UnixStream>,
    wanted: fn(&Delta) -> bool,
) -> Result<Generation, Failed> {
    until(client, PROMPT, |message| match message {
        ToClient::Delta {
            generation,
            payload,
        } => {
            let delta = decode_delta(payload).ok()?;
            wanted(&delta).then_some(*generation)
        }
        _other => None,
    })
    .await
}

/// Waits for the next control message a filter claims, keeping what it passes.
///
/// # Errors
///
/// [`ClientError::Deadline`] when none arrives within `deadline`.
async fn until<Found>(
    client: &mut TestClient<UnixStream>,
    deadline: Duration,
    matching: impl Fn(&ToClient) -> Option<Found>,
) -> Result<Found, Failed> {
    let started = std::time::Instant::now();
    loop {
        let left = deadline
            .checked_sub(started.elapsed())
            .ok_or(ClientError::Deadline { waited: deadline })?;
        if let Received::Control(message) = client.next(left).await?
            && let Some(found) = matching(&message)
        {
            return Ok(found);
        }
    }
}

/// A raw link on a socket pair, with a connection serving the other end, for
/// the cases that must send what no client method sends.
///
/// # Errors
///
/// When the pair cannot be made.
fn raw(host: &mut Host) -> Result<FramedLink<UnixStream>, Failed> {
    let (near, far) = UnixStream::pair()?;
    host.serving
        .push(tokio::spawn(serve(far, Arc::clone(&host.registry))));
    Ok(FramedLink::new(near))
}

/// # Panics
///
/// When a first frame that is not a `Hello` is answered rather than dropped,
/// when a version this server does not speak is not refused with the code that
/// says so, or when a good handshake is not answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handshake_refuses_what_it_cannot_speak() {
    bounded(async {
        let mut host = Host::new()?;

        let mut speaking = raw(&mut host)?;
        speaking
            .send(CHANNEL_CONTROL, &encode_to_server(&ToServer::Ping)?)
            .await?;
        let answered = tokio::time::timeout(PROMPT, speaking.next_frame()).await?;
        assert!(
            matches!(answered, Ok(None) | Err(_)),
            "a first frame that is not a Hello is not answered"
        );

        let mut ancient = raw(&mut host)?;
        let greeting = ToServer::Hello {
            protocol_version: PROTOCOL_VERSION.saturating_add(1),
            client_version: "ancient".to_owned(),
            capabilities: Capabilities::from_bits(0),
        };
        ancient
            .send(CHANNEL_CONTROL, &encode_to_server(&greeting)?)
            .await?;
        let refusal = tokio::time::timeout(PROMPT, ancient.next_frame())
            .await??
            .ok_or("the server said nothing")?
            .payload
            .to_vec();
        assert!(
            matches!(
                decode_to_client(&refusal)?,
                ToClient::Error {
                    code: ErrorCode::ProtocolVersion,
                    ..
                }
            ),
            "a version this server does not speak is refused with the code for it"
        );

        let mut client = host.attach(Capabilities::from_bits(0)).await?;
        client.ping().await?;
        let pong = until(&mut client, PROMPT, |message| {
            matches!(message, ToClient::Pong).then_some(())
        })
        .await;
        assert!(pong.is_ok(), "a good handshake leaves a working connection");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When two clients do not see the same model, or a change one makes does not
/// reach the other with the generation it produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_see_one_model() {
    bounded(async {
        let mut host = Host::new()?;
        let mut one = host.attach(Capabilities::from_bits(0)).await?;
        let mut two = host.attach(Capabilities::from_bits(0)).await?;
        let _made = make_session(&mut one).await?;

        let added = |delta: &Delta| matches!(delta, Delta::SessionAdded { .. });
        let first = delta_of(&mut one, added).await?;
        let second = delta_of(&mut two, added).await?;
        assert_eq!(first, second, "the same change carries the same generation");

        let pane = *host
            .panes()
            .await
            .first()
            .ok_or("the session held no pane")?;
        one.resize(pane, COLUMNS.saturating_sub(1), ROWS).await?;
        let sized = |delta: &Delta| matches!(delta, Delta::PaneResized { .. });
        let here = delta_of(&mut one, sized).await?;
        let there = delta_of(&mut two, sized).await?;
        assert_eq!(here, there, "and a resize by one reaches both");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When input from one client does not reach the pane, or its echo does not
/// reach another client subscribed to the same pane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_reaches_the_pane_and_comes_back() {
    bounded(async {
        let mut host = Host::new()?;
        let mut one = host.attach(Capabilities::from_bits(0)).await?;
        let mut two = host.attach(Capabilities::from_bits(0)).await?;
        let _made = make_session(&mut one).await?;
        let pane = *host
            .panes()
            .await
            .first()
            .ok_or("the session held no pane")?;
        two.subscribe(pane).await?;
        let _channel = until(&mut two, PROMPT, |message| match message {
            ToClient::PaneChannel { channel, .. } => Some(*channel),
            _other => None,
        })
        .await?;

        one.input(pane, b"printf 'from-one\\n'\n".to_vec()).await?;
        let mut watched = Vec::new();
        for _turn in 0..POLL_ATTEMPTS {
            if let Ok(Received::PaneBytes { bytes, .. }) = two.next(PROMPT).await {
                watched.extend_from_slice(&bytes);
            }
            if watched.windows(8).any(|piece| piece == b"from-one") {
                break;
            }
        }
        assert!(
            watched.windows(8).any(|piece| piece == b"from-one"),
            "one client's input came back on another's channel"
        );
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When input past what a stopped pane can hold is dropped rather than
/// reported, or when reporting it closes the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backlog_is_reported_and_the_connection_stays_open() {
    bounded(async {
        let mut host = Host::new()?;
        let mut client = host.attach(Capabilities::from_bits(0)).await?;
        let _made = make_session(&mut client).await?;
        let pane = *host
            .panes()
            .await
            .first()
            .ok_or("the session held no pane")?;

        // A shell that reads nothing, so the pending input has nowhere to go.
        client.input(pane, b"sleep 60\n".to_vec()).await?;
        host.produced(pane, 1).await?;
        let piece = MAXIMUM_PAYLOAD_LENGTH.saturating_sub(INPUT_OVERHEAD);
        let pieces = MAXIMUM_PENDING_INPUT_BYTES
            .checked_div(usize::try_from(piece)?)
            .unwrap_or_default()
            .saturating_add(2);
        for _turn in 0..pieces {
            client
                .input(pane, vec![b'x'; usize::try_from(piece)?])
                .await?;
        }
        let backlog = until(&mut client, PROMPT, |message| match message {
            ToClient::Error { code, message } => {
                (*code == ErrorCode::InputBacklog).then(|| message.clone())
            }
            _other => None,
        })
        .await?;
        assert!(
            backlog.contains(&format!("pane {}", pane.0)),
            "the refusal names the pane: {backlog}"
        );

        client.ping().await?;
        let alive = until(&mut client, PROMPT, |message| {
            matches!(message, ToClient::Pong).then_some(())
        })
        .await;
        assert!(alive.is_ok(), "and the connection stays open");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a well-formed request that is wrong closes the connection instead of
/// being refused, or is refused with the wrong code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_request_is_refused_and_the_connection_stays_open() {
    bounded(async {
        let mut host = Host::new()?;
        let mut client = host.attach(Capabilities::from_bits(0)).await?;

        client.subscribe(PaneId(404)).await?;
        let unknown = until(&mut client, PROMPT, |message| match message {
            ToClient::Error { code, .. } => Some(*code),
            _other => None,
        })
        .await?;
        assert_eq!(unknown, ErrorCode::UnknownPane, "a pane that is not there");

        client.credit(9, 1024).await?;
        let unsubscribed = until(&mut client, PROMPT, |message| match message {
            ToClient::Error { code, .. } => Some(*code),
            _other => None,
        })
        .await?;
        assert_eq!(
            unsubscribed,
            ErrorCode::NotSubscribed,
            "a channel it never had"
        );

        client.ping().await?;
        let alive = until(&mut client, PROMPT, |message| {
            matches!(message, ToClient::Pong).then_some(())
        })
        .await;
        assert!(alive.is_ok(), "and the connection stays open");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a frame that does not decode takes more than its own connection with
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn garbage_closes_only_that_connection() {
    bounded(async {
        let mut host = Host::new()?;
        let mut steady = host.attach(Capabilities::from_bits(0)).await?;
        let mut speaking = raw(&mut host)?;
        let greeting = ToServer::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_version: "garbled".to_owned(),
            capabilities: Capabilities::from_bits(0),
        };
        speaking
            .send(CHANNEL_CONTROL, &encode_to_server(&greeting)?)
            .await?;
        let _reply = tokio::time::timeout(PROMPT, speaking.next_frame()).await??;

        // A discriminant no message claims: well formed as a frame, garbage as
        // a message.
        speaking.send(CHANNEL_CONTROL, &[u8::MAX]).await?;
        let ended = tokio::time::timeout(PROMPT, speaking.next_frame()).await?;
        assert!(
            matches!(ended, Ok(None) | Err(_)),
            "the connection that spoke garbage is closed"
        );

        steady.ping().await?;
        let alive = until(&mut steady, PROMPT, |message| {
            matches!(message, ToClient::Pong).then_some(())
        })
        .await;
        assert!(alive.is_ok(), "and the other client carries on");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// Runs a cursor-position query in a pane's shell and says whether the mirror
/// answered it — the answer is echoed back by the terminal, and nothing else
/// the probe writes carries a capital `R`.
///
/// # Errors
///
/// When the host holds no such pane, or its history cannot be read.
async fn query_answered(host: &Host, pane: PaneId) -> Result<bool, Failed> {
    let before = host.history(pane).await?.len();
    let asked = {
        let held = host.registry.read().await;
        let running = held.pane(pane).ok_or("the host holds no such pane")?;
        running.input(b"printf '\\033[6n'\n".to_vec())
    };
    asked?;
    let mut settled = 0;
    for _attempt in 0..POLL_ATTEMPTS {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = host.history(pane).await?.len();
        if now == settled && now > before {
            break;
        }
        settled = now;
    }
    let history = host.history(pane).await?;
    Ok(history.get(before..).unwrap_or_default().contains(&b'R'))
}

/// # Panics
///
/// When a client that drops without unsubscribing leaves its subscriptions
/// behind, or its channels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_leaves_nothing_behind() {
    bounded(async {
        let mut host = Host::new()?;
        let mut leaving = host.attach(Capabilities::from_bits(0)).await?;
        let _made = make_session(&mut leaving).await?;
        let pane = *host
            .panes()
            .await
            .first()
            .ok_or("the session held no pane")?;
        leaving.subscribe(pane).await?;
        let channel = until(&mut leaving, PROMPT, |message| match message {
            ToClient::PaneChannel { channel, .. } => Some(*channel),
            _other => None,
        })
        .await?;
        assert_eq!(channel, 1, "the first subscription takes the first channel");
        assert!(
            !query_answered(&host, pane).await?,
            "with a subscriber the mirror answers nothing"
        );

        drop(leaving);
        for _attempt in 0..POLL_ATTEMPTS {
            if query_answered(&host, pane).await? {
                break;
            }
        }
        assert!(
            query_answered(&host, pane).await?,
            "with the client gone the mirror answers again"
        );

        let mut arriving = host.attach(Capabilities::from_bits(0)).await?;
        arriving.subscribe(pane).await?;
        let fresh = until(&mut arriving, PROMPT, |message| match message {
            ToClient::PaneChannel { channel: given, .. } => Some(*given),
            _other => None,
        })
        .await?;
        assert_eq!(fresh, 1, "and a new client is given the first channel");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a connection that agreed on compression cannot carry a message, or
/// when one that did not agree engages it anyway.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compression_is_what_both_ends_agreed_on() {
    bounded(async {
        let mut host = Host::new()?;
        for offered in [Capabilities::ZSTD, Capabilities::from_bits(0)] {
            let mut client = host.attach(offered).await?;
            // Were the two ends to disagree about the layer, nothing after the
            // handshake would decode at all.
            client.ping().await?;
            let alive = until(&mut client, PROMPT, |message| {
                matches!(message, ToClient::Pong).then_some(())
            })
            .await;
            assert!(alive.is_ok(), "a link agreed at {offered:?} carries frames");
            let _made = make_session(&mut client).await?;
            let model = client.snapshot().await?;
            assert_eq!(model.sessions.len(), 1, "and a whole model with them");
            let session = model.sessions.first().ok_or("no session")?;
            let closed = SessionCommand::CloseSession {
                session: session.id,
            };
            let _gone = client.command(closed).await?;
        }
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// How many round trips the one-writer case interleaves with a flood.
const ROUND_TRIPS: usize = 1000;

/// # Panics
///
/// When frames from the pump and frames from the loop interleave into
/// anything but a well-formed stream, or when an answer goes missing in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_writer_keeps_the_stream_well_formed() {
    bounded(async {
        let mut host = Host::new()?;
        let mut client = host.attach(Capabilities::from_bits(0)).await?;
        client.auto_credit(true);
        let _made = make_session(&mut client).await?;
        let pane = *host
            .panes()
            .await
            .first()
            .ok_or("the session held no pane")?;

        // A flood that outlasts every round trip, so each one is answered
        // while the pump has something of its own to send.
        let line: String = core::iter::repeat_n('x', COLUMNS.into()).collect();
        let flood = format!("while :; do yes {line} | head -n 2000; done\n");
        client.input(pane, flood.into_bytes()).await?;
        client.subscribe(pane).await?;
        host.produced(pane, u64::try_from(FLOODED_BYTES)?).await?;

        // One at a time, reading each answer before asking again: the client
        // is one task, and a burst it does not read behind fills the socket's
        // own accounting long before its byte count says it should.
        let mut carried = 0_usize;
        for turn in 0..ROUND_TRIPS {
            client.ping().await?;
            loop {
                match client.next(PROMPT).await? {
                    Received::Control(ToClient::Pong) => break,
                    Received::PaneBytes { bytes, .. } => {
                        carried = carried.saturating_add(bytes.len());
                    }
                    Received::Control(_other) => {}
                }
            }
            if turn == 0 {
                assert!(carried > 0, "the flood was already flowing");
            }
        }
        assert!(
            carried > FLOODED_BYTES,
            "every answer arrived while the flood ran: {carried} bytes"
        );
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}
