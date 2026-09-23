//! How far the host answered a pane's terminal queries, told to a client
//! that asked in every `PaneChannel` — and to no other.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::ToClient;
use iznik_server::connection::serve;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::client::{Received, TestClient};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long one read may wait.
const PROMPT: Duration = Duration::from_secs(5);

/// A registry running `sh` in its panes.
///
/// # Errors
///
/// When the mirror thread will not start.
fn daemon() -> Result<Arc<RwLock<Registry>>, Failed> {
    let registry = Registry::new(
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
    );
    Ok(Arc::new(RwLock::new(registry)))
}

/// What the `PaneChannel` a subscription starts with says about answered
/// queries, for a client offering `capabilities`.
///
/// # Errors
///
/// When the handshake, the session or the announcement fails.
async fn announced(
    registry: &Arc<RwLock<Registry>>,
    capabilities: Capabilities,
) -> Result<(Sequence, Option<Sequence>), Failed> {
    let (near, far) = UnixStream::pair()?;
    let _serving = tokio::spawn(serve(far, Arc::clone(registry)));
    let mut client = TestClient::over(near);
    let _greeting = client.hello(capabilities).await?;
    let outcome = client
        .command(SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: 80,
            rows: 24,
            working_directory: None,
        })
        .await?;
    if !matches!(outcome, CommandOutcome::Applied { .. }) {
        return Err(format!("the session was refused: {outcome:?}").into());
    }
    let model = client.snapshot().await?;
    let pane: PaneId = model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .map(|held| held.id)
        .next_back()
        .ok_or("the session has a pane")?;
    client.subscribe(pane).await?;
    loop {
        if let Received::Control(ToClient::PaneChannel {
            pane: named,
            sequence,
            answered_through,
            ..
        }) = client.next(PROMPT).await?
            && named == pane
        {
            return Ok((sequence, answered_through));
        }
    }
}

/// # Panics
///
/// When a client that asked is not told, is told something past the screen
/// it starts from, or a client that did not ask is sent the field.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answered_queries_are_told_only_to_a_client_that_asked() {
    let case = async {
        let registry = daemon()?;
        let (sequence, answered) = announced(&registry, Capabilities::ANSWERED).await?;
        let answered = answered.ok_or("a client that asked is told")?;
        assert!(
            answered <= sequence,
            "nothing past the screen a cold subscription starts from was answered: \
             {answered:?} against {sequence:?}"
        );
        let (_sequence, older) = announced(&registry, Capabilities::from_bits(0)).await?;
        assert_eq!(
            older, None,
            "a client that did not ask is not sent the field"
        );
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .unwrap_or_else(|_late| Err("the case ran past its deadline".into()))
        .unwrap_or_else(|error| panic!("{error}"));
}
