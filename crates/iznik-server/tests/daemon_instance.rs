//! The daemon instance a connection announces: the same for every connection
//! to one registry, different for another, and sent only to a client that
//! said it can read it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::capabilities::Capabilities;
use iznik_server::connection::serve;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::client::{ServerHello, TestClient};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// A registry holding nothing: one daemon's worth of state.
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

/// What a connection to `registry` greets a client offering `capabilities`
/// with.
///
/// # Errors
///
/// When the pair cannot be made or the handshake fails.
async fn greeting(
    registry: &Arc<RwLock<Registry>>,
    capabilities: Capabilities,
) -> Result<ServerHello, Failed> {
    let (near, far) = UnixStream::pair()?;
    let _serving = tokio::spawn(serve(far, Arc::clone(registry)));
    let mut client = TestClient::over(near);
    Ok(client.hello(capabilities).await?)
}

/// # Panics
///
/// When a daemon names itself differently to two clients, the same as another
/// daemon, or to a client that cannot read the field.
#[tokio::test]
async fn daemon_instance_is_one_per_daemon_and_only_for_a_client_that_asked() {
    let case = async {
        let first = daemon()?;
        let second = daemon()?;
        let one = greeting(&first, Capabilities::INSTANCE).await?;
        let again = greeting(&first, Capabilities::INSTANCE).await?;
        let other = greeting(&second, Capabilities::INSTANCE).await?;
        let older = greeting(&first, Capabilities::from_bits(0)).await?;
        assert!(one.instance.is_some(), "a client that asked is told");
        assert!(one.capabilities.contains(Capabilities::INSTANCE));
        assert_eq!(one.instance, again.instance, "one daemon is one instance");
        assert_ne!(one.instance, other.instance, "another daemon is another");
        assert_eq!(
            older.instance, None,
            "a client that did not ask is not sent it"
        );
        assert!(
            !older.capabilities.contains(Capabilities::INSTANCE),
            "and is not told the field is there"
        );
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .unwrap_or_else(|_late| Err("the case ran past its deadline".into()))
        .unwrap_or_else(|error| panic!("{error}"));
}
