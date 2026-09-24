//! Keystrokes a pane has no room for are held, never refused part way: a
//! paste longer than one message and longer than a busy pane's input can hold
//! reaches the program whole, while the connection keeps answering.
//!
//! Over a socket pair, like the other connection cases: no daemon, no socket
//! file. The pane runs `sh`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::{MAXIMUM_INPUT_LENGTH, ToClient};
use iznik_server::connection::serve;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::pty::streams::MAXIMUM_PENDING_INPUT_BYTES;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::client::{ClientError, Received, TestClient};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

/// The deadline the case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_mins(1);
/// The deadline for something that should already be on its way.
const PROMPT: Duration = Duration::from_secs(2);
/// How long the case waits between looks at the pane.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Anything the case can fail on.
type Failed = Box<dyn std::error::Error>;

/// Waits until the pane's history holds `wanted`, within [`DEADLINE`].
///
/// # Errors
///
/// When it does not, or the host holds no such pane.
async fn shown(registry: &RwLock<Registry>, pane: PaneId, wanted: &[u8]) -> Result<(), Failed> {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let history = registry
                .read()
                .await
                .pane(pane)
                .ok_or("the host holds no such pane")?
                .read_history(Sequence(0))?;
            if history.windows(wanted.len()).any(|window| window == wanted) {
                return Ok::<(), Failed>(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await?
}

/// # Panics
///
/// When a paste larger than a busy pane's input can hold loses any of its
/// middle — a piece refused while the pane was busy, and the next one taken
/// once it had drained — or when holding it stops the connection answering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_input_a_paste_past_the_backlog_arrives_whole() {
    let case = async {
        let registry = Arc::new(RwLock::new(Registry::new(
            RegistryDefaults {
                program: Program::Command {
                    path: "sh".into(),
                    arguments: Vec::new(),
                },
                terminfo_directory: None,
                agent_socket: None,
                program_interval: Duration::ZERO,
            },
            Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES))),
            MirrorThread::start()?,
        )));
        let (near, far) = UnixStream::pair()?;
        let _serving = tokio::spawn(serve(far, Arc::clone(&registry)));
        let mut client = TestClient::over(near);
        let _greeting = client.hello(Capabilities::from_bits(0)).await?;
        let made = client
            .command(SessionCommand::CreateSession {
                name: "work".to_owned(),
                columns: 80,
                rows: 24,
                working_directory: None,
            })
            .await?;
        assert!(matches!(made, CommandOutcome::Applied { .. }), "{made:?}");
        let pane = registry
            .read()
            .await
            .snapshot()
            .sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .map(|held| held.id)
            .next()
            .ok_or("the session held no pane")?;
        let piece = usize::try_from(MAXIMUM_INPUT_LENGTH)?;
        let pieces = MAXIMUM_PENDING_INPUT_BYTES
            .checked_div(piece)
            .unwrap_or_default()
            .saturating_add(3);
        let total = piece.saturating_mul(pieces);

        // A raw terminal, so nothing is line-edited or echoed; a shell busy
        // long enough for the paste to back up; then every byte counted. The
        // count is the shell's arithmetic, so the echo of the line lacks it.
        let script = format!(
            "stty raw -echo; echo RE''ADY; sleep 3; head -c $(({piece}*{pieces})) | wc -c; echo DO''NE\n"
        );
        client.input(pane, script.into_bytes()).await?;
        shown(&registry, pane, b"READY").await?;
        for _turn in 0..pieces {
            client.input(pane, vec![b'x'; piece]).await?;
        }

        client.ping().await?;
        let answered = loop {
            match client.next(PROMPT).await? {
                Received::Control(ToClient::Pong) => break None,
                Received::Control(ToClient::Error { message, .. }) => break Some(message),
                _other => {}
            }
        };
        assert_eq!(answered, None, "nothing is refused, and pings are answered");

        shown(&registry, pane, b"DONE").await?;
        shown(&registry, pane, total.to_string().as_bytes()).await?;
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .unwrap_or_else(|_late| Err(ClientError::Deadline { waited: DEADLINE }.into()))
        .unwrap_or_else(|error| panic!("{error}"));
}
