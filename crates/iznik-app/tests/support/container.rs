//! A headless application's private byte relay into the two-container fixture.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_app::bridge::EngineBridge;
use iznik_client::host::manager::ManagerOptions;
use iznik_client::host::state::BackoffPolicy;
use iznik_client::transport::ClientRuntimePaths;
use iznik_harness::fixture::{ENGINE_ALIAS, Fixture, FixtureOptions, MOUNT_POINT};
use iznik_harness::process::Deadline;
use iznik_harness::staging::{self, STAGING_DEADLINE};
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::watch;

/// One stream cannot outlive a bounded regression case.
const CONNECTION_DEADLINE: Duration = Duration::from_secs(90);
/// Runtime cancellation is bounded even when a peer stops answering.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(1);
/// Reconnection is prompt enough to observe without waiting on product backoff.
const BACKOFF: Duration = Duration::from_millis(20);
/// Container SSH startup receives a real transport budget, independent of UI polling.
const GREETING_DEADLINE: Duration = Duration::from_secs(5);

/// Fixture setup failures are propagated to the owning test.
type Failed = Box<dyn std::error::Error>;
/// Relay failures cross runtime threads only as owned text.
type RelayFailure = Box<dyn std::error::Error + Send + Sync>;

/// All relay timing is explicit so a caller can shorten it.
#[derive(Clone, Debug)]
pub(crate) struct Options {
    /// Maximum lifetime of one connected SSH relay.
    pub(crate) connection_deadline: Duration,
    /// Maximum time to reap cancelled runtime tasks.
    pub(crate) shutdown_deadline: Duration,
    /// Reconnection delay after an intentionally cut link.
    pub(crate) backoff: Duration,
    /// Maximum wait for the fixture's SSH process to greet the engine.
    pub(crate) greeting_deadline: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connection_deadline: CONNECTION_DEADLINE,
            shutdown_deadline: SHUTDOWN_DEADLINE,
            backoff: BACKOFF,
            greeting_deadline: GREETING_DEADLINE,
        }
    }
}

/// Own the relay before its containers and keep every host path private to this test.
pub(crate) struct Container {
    /// Taken and shut down before the container owner can drop.
    runtime: Option<Runtime>,
    /// Keeps credentials, server and SSH process alive until relay shutdown.
    fixture: Arc<Fixture>,
    /// Local relay socket and empty engine runtime directories.
    directory: PathBuf,
    /// Link cuts cancel existing sessions; reconnects admit fresh SSH processes.
    connected: watch::Sender<bool>,
    /// Unexpected relay errors retained for deadline diagnostics.
    failures: Arc<Mutex<Vec<String>>>,
    /// Explicit timing for relay and engine owners.
    options: Options,
}

impl Container {
    /// Start the repository fixture and a Unix listener with no developer SSH access.
    ///
    /// # Errors
    /// Returns staging, container, filesystem or runtime startup failures.
    pub(crate) fn start(options: Options) -> Result<Self, Failed> {
        let staged = staging::stage(Deadline(STAGING_DEADLINE))?;
        let fixture = Arc::new(Fixture::start(FixtureOptions::new(1, staged))?);
        let directory = std::env::temp_dir().join(format!("window-{}", fixture.prefix()));
        std::fs::create_dir_all(&directory)?;
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        let listener = runtime.block_on(async { UnixListener::bind(directory.join("relay")) })?;
        let (connected, status) = watch::channel(true);
        let failures = Arc::new(Mutex::new(Vec::new()));
        runtime.spawn(accept(
            listener,
            Arc::clone(&fixture),
            status,
            Arc::clone(&failures),
            options.connection_deadline,
        ));
        Ok(Self {
            runtime: Some(runtime),
            fixture,
            directory,
            connected,
            failures,
            options,
        })
    }

    /// The production engine's alias for this test-owned relay socket.
    pub(crate) fn alias(&self) -> String {
        format!("unix:{}", self.directory.join("relay").display())
    }

    /// Give each window an independent production engine and runtime directory.
    ///
    /// # Errors
    /// Returns path or engine startup failures.
    pub(crate) fn bridge(&self, name: &str) -> Result<EngineBridge, Failed> {
        let artifacts = self.directory.join("artifacts");
        std::fs::create_dir_all(&artifacts)?;
        let paths = ClientRuntimePaths::under(&self.directory.join(name))?;
        let mut options = ManagerOptions::new(artifacts, paths);
        options.backoff = BackoffPolicy {
            initial: self.options.backoff,
            maximum: self.options.backoff,
            ..BackoffPolicy::default()
        };
        options.channel.greeting_deadline = self.options.greeting_deadline;
        Ok(EngineBridge::under(options)?)
    }

    /// Run a bounded control command in this fixture's isolated host container.
    ///
    /// # Errors
    /// Returns the container command's failure or deadline error.
    pub(crate) fn run_host(&self, command: &str, deadline: Duration) -> Result<(), Failed> {
        self.fixture
            .exec(&Fixture::host_alias(0), command, deadline)?;
        Ok(())
    }

    /// Cut all current byte streams or permit fresh fixture SSH sessions.
    pub(crate) fn set_connected(&self, connected: bool) {
        self.connected.send_replace(connected);
    }

    /// Owned diagnostics can accompany a failed UI observation deadline.
    pub(crate) fn failures(&self) -> Vec<String> {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        self.connected.send_replace(false);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(self.options.shutdown_deadline);
        }
        let _removed = std::fs::remove_dir_all(&self.directory);
    }
}

/// Accept isolated client connections; a cut link never touches another test's fixture.
async fn accept(
    listener: UnixListener,
    fixture: Arc<Fixture>,
    mut connected: watch::Receiver<bool>,
    failures: Arc<Mutex<Vec<String>>>,
    deadline: Duration,
) {
    loop {
        if !*connected.borrow_and_update() {
            if connected.changed().await.is_err() {
                break;
            }
            continue;
        }
        let accepted = tokio::select! {
            changed = connected.changed() => {
                if changed.is_err() { break; }
                continue;
            }
            accepted = listener.accept() => accepted,
        };
        let Ok((socket, _address)) = accepted else {
            break;
        };
        let fixture = Arc::clone(&fixture);
        let connected = connected.clone();
        let failures = Arc::clone(&failures);
        tokio::spawn(async move {
            if let Err(error) = relay(socket, &fixture, connected, deadline).await {
                failures
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(error.to_string());
            }
        });
    }
}

/// Copy bytes through a real SSH process whose credentials never leave the engine container.
///
/// # Errors
/// Returns container execution, pipe, stream or deadline failures.
async fn relay(
    mut socket: UnixStream,
    fixture: &Fixture,
    mut connected: watch::Receiver<bool>,
    deadline: Duration,
) -> Result<(), RelayFailure> {
    if !*connected.borrow_and_update() {
        return Ok(());
    }
    let command = format!("exec ssh -o BatchMode=yes host0 {MOUNT_POINT}/bin/iznik-server --stdio");
    let mut command =
        tokio::process::Command::from(fixture.stream_command(ENGINE_ALIAS, &command)?);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().ok_or("missing relay input")?;
    let mut output = child.stdout.take().ok_or("missing relay output")?;
    let (mut reading, mut writing) = socket.split();
    let copying = async {
        tokio::try_join!(
            tokio::io::copy(&mut reading, &mut input),
            tokio::io::copy(&mut output, &mut writing)
        )
    };
    tokio::select! {
        _changed = connected.changed() => {},
        result = tokio::time::timeout(deadline, copying) => { result??; },
        result = child.wait() => { result?; },
    }
    Ok(())
}
