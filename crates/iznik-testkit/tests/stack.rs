//! The harness every later integration test is written against, held to what
//! it promises: a daemon that answers within its ceiling, a client that can
//! do a session's whole round trip through it, and nothing left behind when
//! it goes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, Created, SessionCommand};
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::ToClient;
use iznik_server::multiplexer::credit::{FRAME_PAYLOAD_LENGTH, INITIAL_CREDIT_BYTES};
use iznik_testkit::client::{Received, TestClient};
use iznik_testkit::stack::{DaemonMode, STARTUP_CEILING, Stack, StackError, StackOptions};
use iznik_testkit::vt::Vt;
use tokio::net::UnixStream;

/// The size a case's panes are made at.
const COLUMNS: u16 = 80;

/// Their height.
const ROWS: u16 = 24;

/// How long a case waits for something the stack should do at once.
const PROMPT: Duration = Duration::from_secs(5);

/// How long a case waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How many looks it takes before it gives up.
const POLL_ATTEMPTS: usize = 200;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A stack and a client already shaken hands with it.
///
/// # Errors
///
/// When the stack will not start or the handshake fails.
async fn attached() -> Result<(Stack, TestClient<UnixStream>), Failed> {
    let stack = Stack::start(StackOptions::default()).await?;
    let mut client = TestClient::connect(stack.socket()).await?;
    let _greeting = client.hello(Capabilities::from_bits(0)).await?;
    Ok((stack, client))
}

/// The pane a fresh session made.
///
/// # Errors
///
/// When the command is refused, or the model holds no pane after it.
async fn one_pane(client: &mut TestClient<UnixStream>) -> Result<PaneId, Failed> {
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
    let model = client.snapshot().await?;
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .map(|pane| pane.id)
        .next()
        .ok_or_else(|| "the model holds no pane".into())
}

/// # Panics
///
/// When a stack does not answer within its ceiling, or leaves its socket and
/// its directory behind when it goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stack_starts_within_its_ceiling_and_leaves_nothing() {
    let case = async {
        let started = Instant::now();
        let stack = Stack::start(StackOptions::default()).await?;
        let taken = started.elapsed();
        assert!(
            taken < STARTUP_CEILING,
            "it answered in {taken:?}, inside {STARTUP_CEILING:?}"
        );
        let socket = stack.socket().to_path_buf();
        let directory = stack.directory().to_path_buf();
        assert!(
            UnixStream::connect(&socket).await.is_ok(),
            "and a client can reach it"
        );

        drop(stack);
        for _attempt in 0..POLL_ATTEMPTS {
            if !directory.exists() {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        assert!(!socket.exists(), "its socket goes with it");
        assert!(!directory.exists(), "and its runtime directory");
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a binary that is not there is waited for rather than reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_binary_that_is_not_there_is_named_and_not_waited_for() {
    let case = async {
        let missing = PathBuf::from("/nonexistent/iznik-server");
        let started = Instant::now();
        let refused = Stack::start(StackOptions {
            daemon: DaemonMode::Binary(missing.clone()),
            ..StackOptions::default()
        })
        .await;
        let taken = started.elapsed();
        let Err(StackError::Binary { path, .. }) = refused else {
            return Err(format!("it did not name the binary: {refused:?}").into());
        };
        assert_eq!(path, missing, "it names the path it was given");
        assert!(
            taken < STARTUP_CEILING,
            "and says so at once rather than waiting: {taken:?}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a session's round trip through the stack does not reach the shell and
/// come back, or the screen and the bytes after it do not reassemble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_makes_a_round_trip_through_the_stack() {
    let case = async {
        let (_stack, mut client) = attached().await?;
        let pane = one_pane(&mut client).await?;

        // A `SessionAdded` is still on its way; what matters is the order of
        // the two the subscription itself produces.
        client.subscribe(pane).await?;
        let opened = until(&mut client, |message| match message {
            ToClient::PaneChannel { channel, .. } => Some(Told::Channel(*channel)),
            ToClient::Screen { bytes, .. } => Some(Told::Screen(bytes.clone())),
            _other => None,
        })
        .await?;
        let Told::Channel(channel) = opened else {
            return Err("the truth came before the channel it flows on".into());
        };
        assert!(channel > 0, "and on a pane channel, never the control one");
        let Told::Screen(bytes) = until(&mut client, |message| match message {
            ToClient::Screen { bytes, .. } => Some(Told::Screen(bytes.clone())),
            _other => None,
        })
        .await?
        else {
            return Err("the truth did not follow it".into());
        };

        client.auto_credit(true);
        client.input(pane, b"echo harness\n".to_vec()).await?;
        for _attempt in 0..POLL_ATTEMPTS {
            let _received = client.next(PROMPT).await?;
            if contains(client.bytes_of(pane), b"harness") {
                break;
            }
        }
        let mut oracle = Vt::new(COLUMNS, ROWS)?;
        oracle.feed(&bytes);
        oracle.feed(client.bytes_of(pane));
        let shown = oracle.screen_text()?;
        assert!(
            shown.contains("harness"),
            "the screen and the bytes after it show the echoed line: {shown:?}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// The two things a subscription opens with.
#[derive(Clone, Debug)]
enum Told {
    /// The channel it will flow on.
    Channel(u8),
    /// The truth it begins from.
    Screen(Vec<u8>),
}

/// The first control message a filter claims.
///
/// # Errors
///
/// When none arrives within [`PROMPT`].
async fn until<Found>(
    client: &mut TestClient<UnixStream>,
    matching: impl Fn(&ToClient) -> Option<Found>,
) -> Result<Found, Failed> {
    let started = Instant::now();
    while started.elapsed() < PROMPT {
        if let Received::Control(message) = client.next(PROMPT).await?
            && let Some(found) = matching(&message)
        {
            return Ok(found);
        }
    }
    Err(format!("nothing claimed within {PROMPT:?}").into())
}

/// Whether a byte string holds another.
fn contains(held: &[u8], wanted: &[u8]) -> bool {
    held.windows(wanted.len()).any(|piece| piece == wanted)
}

/// How much a case floods a pane with, which is more than one window.
const FLOOD_MEBIBYTES: usize = 1;

/// The unit that flood is stated in.
const MEBIBYTE: usize = 1024 * 1024;

/// The deadline a case gives bytes it does not expect.
const BRIEF: Duration = Duration::from_millis(300);

/// Asks a pane's shell to write about `mebibytes` mebibytes of printable
/// lines.
///
/// # Errors
///
/// When the pane will not take it.
async fn flood(
    client: &mut TestClient<UnixStream>,
    pane: PaneId,
    mebibytes: usize,
) -> Result<(), Failed> {
    let line: String = core::iter::repeat_n('x', COLUMNS.into()).collect();
    let width = usize::from(COLUMNS).saturating_add(1);
    let wanted = MEBIBYTE.saturating_mul(mebibytes).saturating_add(width);
    let count = wanted.checked_div(width).unwrap_or_default();
    let asked = format!("yes {line} | head -n {count}\n");
    client.input(pane, asked.into_bytes()).await?;
    Ok(())
}

/// Reads until nothing more arrives, and says how many pane bytes came.
async fn drain(client: &mut TestClient<UnixStream>, pane: PaneId) -> usize {
    for _attempt in 0..POLL_ATTEMPTS {
        if client.next(BRIEF).await.is_err() {
            break;
        }
    }
    client.bytes_of(pane).len()
}

/// # Panics
///
/// When a client that returns credit does not keep a flood flowing, or one
/// that does not is sent more than the window it was given.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credit_is_what_keeps_a_flood_flowing() {
    let case = async {
        let (_stack, mut client) = attached().await?;
        let pane = one_pane(&mut client).await?;
        client.subscribe(pane).await?;
        let channel = until(&mut client, |message| match message {
            ToClient::PaneChannel { channel, .. } => Some(*channel),
            _other => None,
        })
        .await?;

        // Nothing returned: the flow stops at the window the server gave it.
        flood(&mut client, pane, FLOOD_MEBIBYTES).await?;
        let held = drain(&mut client, pane).await;
        let window = usize::try_from(INITIAL_CREDIT_BYTES)?;
        assert_eq!(
            held, window,
            "a silent client is sent its window and no more"
        );

        // Returned as it arrives, once something has arrived to return it
        // for: automatic credit answers frames, so a window that has run out
        // has to be opened by hand before it can keep itself open.
        client.auto_credit(true);
        client.credit(channel, FRAME_PAYLOAD_LENGTH).await?;
        let whole = drain(&mut client, pane).await;
        assert!(
            whole > window.saturating_add(usize::try_from(FRAME_PAYLOAD_LENGTH)?),
            "one credit moved it and the client's own kept it moving: {held} to {whole}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
