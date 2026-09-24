//! The daemon instance a connection announces: the same for every connection
//! to one registry, different for another, and sent only to a client that
//! said it can read it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::BuildDigest;
use iznik_server::connection::serve;
use iznik_server::daemon::build_beside;
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

/// The digest a bootstrap writes beside a server, as its first line.
const WRITTEN: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// What the next build's upload writes over it while the daemon runs.
const REPLACED: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// # Panics
///
/// When a daemon does not announce the build read beside its binary when it
/// started — and that one, even after the file is replaced — or announces it
/// to a client that cannot read it.
#[tokio::test]
async fn daemon_instance_names_the_build_read_when_it_started() {
    let case = async {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let directory = base.join(format!("iznik-build-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let binary = directory.join("iznik-server");
        std::fs::write(
            directory.join("iznik-server.sha256"),
            format!("{WRITTEN}\n"),
        )?;
        let read = build_beside(&binary);
        let started = Arc::try_unwrap(daemon()?)
            .map_err(|_shared| "the registry is shared")?
            .into_inner()
            .built_from(read);
        let registry = Arc::new(RwLock::new(started));
        // The next build is installed while this daemon runs.
        std::fs::write(
            directory.join("iznik-server.sha256"),
            format!("{REPLACED}\n"),
        )?;
        let both =
            Capabilities::from_bits(Capabilities::INSTANCE.bits() | Capabilities::BUILD.bits());
        let told = greeting(&registry, both).await?;
        let mut expected = [0_u8; 32];
        for (at, byte) in expected.iter_mut().enumerate() {
            *byte = u8::try_from(at)?;
        }
        assert_eq!(
            told.build,
            Some(BuildDigest(expected)),
            "the build it started from, not the one written since"
        );
        assert!(told.capabilities.contains(Capabilities::BUILD));
        let only_instance = greeting(&registry, Capabilities::INSTANCE).await?;
        assert_eq!(
            only_instance.build, None,
            "a client that did not ask is not sent it"
        );
        assert!(!only_instance.capabilities.contains(Capabilities::BUILD));
        let unknown = greeting(&daemon()?, both).await?;
        assert_eq!(
            unknown.build, None,
            "a daemon that does not know says nothing"
        );
        assert!(
            !unknown.capabilities.contains(Capabilities::BUILD),
            "and does not claim the field"
        );
        assert_eq!(
            build_beside(&directory.join("elsewhere")),
            None,
            "a binary with nothing beside it has no build to name"
        );
        let _gone = std::fs::remove_dir_all(&directory);
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .unwrap_or_else(|_late| Err("the case ran past its deadline".into()))
        .unwrap_or_else(|error| panic!("{error}"));
}
