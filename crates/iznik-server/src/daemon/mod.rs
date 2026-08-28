//! The daemon: runtime paths, the single-instance lock, the accept loop, idle
//! shutdown, logging, and the `--daemon`, `--foreground`, `--stop` and
//! `--version` entry points.
//!
//! The daemon *is* the sessions. It owns the registry and every pane, accepts
//! any number of clients, and is indifferent to whether anyone is attached —
//! which is the whole product: a dropped link costs a reconnect, not a
//! session. Everything here exists to make that indifference true on a real
//! host: a runtime directory a locked-down machine will allow, a lock rather
//! than a socket file for single instance, a session of its own so an SSH
//! disconnect cannot hang it up, an exit when it has nothing to hold, and a
//! log that cannot fill a disk.
//!
//! Every timing is a field of [`DaemonOptions`], so no test waits ten minutes
//! to watch an idle daemon go.

pub mod idle;
pub mod lock;
pub mod logging;
pub mod socket;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex as Blocking;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use core::fmt::{self, Display, Formatter};

use iznik_protocol::message::PROTOCOL_VERSION;
use nix::sys::signal::{self, Signal};
use nix::unistd::{Pid, Uid, setsid};
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, watch};

use crate::connection;
use crate::daemon::idle::{
    IDLE_CHECK_INTERVAL, IDLE_SHUTDOWN, Idle, SOCKET_POLL_INTERVAL, SOCKET_READY_CAP, STOP_CAP,
};
use crate::daemon::lock::{Lock, LockError};
use crate::daemon::socket::SocketError;
use crate::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use crate::pty::spawn::Program;
use crate::session::registry::{Registry, RegistryDefaults};
use crate::terminal::mirror::{MirrorError, MirrorThread};

/// This server's own version, which `--version` prints and the bootstrap of
/// plan 0005 parses.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The directory the runtime files live in, under whichever base is allowed.
const DIRECTORY_NAME: &str = "iznik";

/// The socket every client connects to, inside that directory.
const SOCKET_NAME: &str = "server.sock";

/// The lock that enforces a single instance, beside it.
const LOCK_NAME: &str = "server.lock";

/// The log, beside both.
const LOG_NAME: &str = "server.log";

/// The mode the runtime directory is created with: the owner's, and nobody
/// else's — a socket anyone can connect to is a shell anyone can have.
const OWNER_ONLY: u32 = 0o700;

/// The flag that shortens the idle interval, so a test can watch a daemon go.
const IDLE_FLAG: &str = "--idle-shutdown-seconds";

/// The subcommand each entry point answers to.
const DAEMON: &str = "--daemon";

/// Runs the accept loop in the calling process.
const FOREGROUND: &str = "--foreground";

/// Ends a running daemon.
const STOP: &str = "--stop";

/// Prints the versions and nothing else.
const VERSION: &str = "--version";

/// Where the daemon's files live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePaths {
    /// The directory holding all three.
    pub directory: PathBuf,
    /// The socket clients connect to.
    pub socket: PathBuf,
    /// The file whose lock enforces a single instance.
    pub lock: PathBuf,
    /// The log file.
    pub log: PathBuf,
}

/// Why the runtime paths could not be settled.
#[derive(Debug)]
pub enum PathsError {
    /// The directory could not be created or its mode could not be set.
    Io {
        /// The directory.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
}

impl Display for PathsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            PathsError::Io { path, source } => {
                write!(
                    formatter,
                    "the runtime directory {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl core::error::Error for PathsError {}

impl RuntimePaths {
    /// The paths under `directory`, which is created with mode `0700` if it is
    /// not there.
    ///
    /// # Errors
    ///
    /// [`PathsError::Io`] when the directory cannot be created or restricted.
    pub fn under(directory: &Path) -> Result<RuntimePaths, PathsError> {
        std::fs::create_dir_all(directory).map_err(|source| PathsError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        std::fs::set_permissions(
            directory,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(OWNER_ONLY),
        )
        .map_err(|source| PathsError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        Ok(RuntimePaths {
            socket: directory.join(SOCKET_NAME),
            lock: directory.join(LOCK_NAME),
            log: directory.join(LOG_NAME),
            directory: directory.to_path_buf(),
        })
    }

    /// The paths this host allows: under `XDG_RUNTIME_DIR` when it is set,
    /// else under `TMPDIR`, else under `/tmp`, each in a directory of this
    /// user's own.
    ///
    /// A locked-down host with no runtime directory is a real case, and one
    /// that must not be discovered in the middle of somebody's first bootstrap.
    ///
    /// # Errors
    ///
    /// As [`RuntimePaths::under`].
    pub fn resolve() -> Result<RuntimePaths, PathsError> {
        RuntimePaths::under(&base())
    }
}

/// The directory the runtime files belong in on this host.
fn base() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|held| !held.is_empty()) {
        return PathBuf::from(runtime).join(DIRECTORY_NAME);
    }
    let temporary = std::env::var_os("TMPDIR")
        .filter(|held| !held.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from);
    temporary.join(format!("{DIRECTORY_NAME}-{}", Uid::current().as_raw()))
}

/// Every timing and default the daemon runs under, so a test can shorten any
/// of them.
#[derive(Clone, Debug)]
pub struct DaemonOptions {
    /// How long with no panes and no clients before it exits.
    pub idle_shutdown: Duration,
    /// How long `--daemon` waits for the socket to answer.
    pub socket_ready_cap: Duration,
    /// How long `--stop` waits for the socket to go.
    pub stop_cap: Duration,
    /// What a pane runs.
    pub program: Program,
}

impl Default for DaemonOptions {
    fn default() -> DaemonOptions {
        DaemonOptions {
            idle_shutdown: IDLE_SHUTDOWN,
            socket_ready_cap: SOCKET_READY_CAP,
            stop_cap: STOP_CAP,
            program: Program::LoginShell,
        }
    }
}

/// Why a daemon could not run.
#[derive(Debug)]
pub enum DaemonError {
    /// The runtime paths could not be settled.
    Paths(PathsError),
    /// Another daemon holds the lock, or the lock file failed.
    Lock(LockError),
    /// The socket could not be bound.
    Socket(SocketError),
    /// The mirror thread could not be started.
    Mirror(MirrorError),
    /// The accept loop failed.
    Io {
        /// What the operating system said.
        source: std::io::Error,
    },
}

impl Display for DaemonError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::Paths(source) => write!(formatter, "{source}"),
            DaemonError::Lock(source) => write!(formatter, "{source}"),
            DaemonError::Socket(source) => write!(formatter, "{source}"),
            DaemonError::Mirror(source) => write!(formatter, "the mirror thread: {source}"),
            DaemonError::Io { source } => write!(formatter, "the accept loop: {source}"),
        }
    }
}

impl core::error::Error for DaemonError {}

impl From<PathsError> for DaemonError {
    fn from(source: PathsError) -> DaemonError {
        DaemonError::Paths(source)
    }
}

impl From<LockError> for DaemonError {
    fn from(source: LockError) -> DaemonError {
        DaemonError::Lock(source)
    }
}

impl From<SocketError> for DaemonError {
    fn from(source: SocketError) -> DaemonError {
        DaemonError::Socket(source)
    }
}

impl From<MirrorError> for DaemonError {
    fn from(source: MirrorError) -> DaemonError {
        DaemonError::Mirror(source)
    }
}

/// Hands one accepted stream to a connection of its own, counted while it
/// lives. One refused accept is not the end of the daemon: the peer went away
/// between knocking and being let in.
///
/// # Panics
///
/// It spawns, so it must be called from within a Tokio runtime.
async fn admit(
    accepted: Result<(tokio::net::UnixStream, tokio::net::unix::SocketAddr), std::io::Error>,
    registry: &Arc<RwLock<Registry>>,
    attached: &Attached,
) {
    let (stream, _address) = match accepted {
        Ok(accepted) => accepted,
        Err(error) => {
            // A peer that went away between knocking and being let in fails
            // once; a descriptor table that is full fails instantly and for
            // ever, and without this pause the loop would spin on it and
            // starve the idle check as well.
            tracing::warn!(%error, "a connection could not be accepted");
            tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
            return;
        }
    };
    let arrived = attached.arrive();
    let host = Arc::clone(registry);
    let _serving = tokio::spawn(async move {
        if let Err(error) = connection::serve(stream, host).await {
            tracing::info!(%error, "a client's connection ended");
        }
        drop(arrived);
    });
}

/// Whether a change on the shutdown channel says to stop. A channel whose
/// senders are all gone can never say so again, and is not asked again.
fn told(
    changed: &Result<(), watch::error::RecvError>,
    shutdown: &mut watch::Receiver<bool>,
    asked: &mut bool,
) -> bool {
    if changed.is_err() {
        *asked = false;
        return false;
    }
    *shutdown.borrow_and_update()
}

/// Turns what the panes have reported into deltas, records what the daemon is
/// holding, and says whether it has held nothing for long enough to go.
async fn spent(
    registry: &Arc<RwLock<Registry>>,
    attached: &Attached,
    idling: &mut Idle,
    interval: Duration,
) -> bool {
    registry.write().await.ingest();
    idling.observe(panes_of(registry).await, attached.busy());
    idling.expired(interval)
}

/// How many panes the host holds, which with the client count is what idleness
/// is made of.
async fn panes_of(registry: &Arc<RwLock<Registry>>) -> usize {
    registry
        .read()
        .await
        .snapshot()
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .fold(0, |held, tab| held.saturating_add(tab.panes.len()))
}

/// How many clients a daemon is serving, so it knows when it has nobody.
#[derive(Clone, Debug, Default)]
struct Attached {
    /// The count, shared with every connection task.
    count: Arc<AtomicUsize>,
    /// How many have arrived since idleness was last looked at. A count taken
    /// once a second would miss a client that connects and goes between two
    /// looks — a readiness probe is exactly that — and a daemon it had served
    /// all day would exit as if nobody had touched it.
    arrivals: Arc<AtomicUsize>,
}

impl Attached {
    /// A guard that counts one client while it lives.
    fn arrive(&self) -> Arrived {
        let _before = self.count.fetch_add(1, Ordering::Relaxed);
        let _arrived = self.arrivals.fetch_add(1, Ordering::Relaxed);
        Arrived {
            count: Arc::clone(&self.count),
        }
    }

    /// How many are attached now, and how many came and went since the last
    /// look, which together are what says the daemon was not idle.
    fn busy(&self) -> usize {
        self.count
            .load(Ordering::Relaxed)
            .saturating_add(self.arrivals.swap(0, Ordering::Relaxed))
    }
}

/// One attached client, counted until it is dropped.
#[derive(Debug)]
struct Arrived {
    /// The count it belongs to.
    count: Arc<AtomicUsize>,
}

impl Drop for Arrived {
    fn drop(&mut self) {
        let _before = self.count.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The daemon: bind, accept, serve, and go when there is nothing left to hold.
///
/// It exits `Ok` when `shutdown` flips, when `SIGTERM` arrives, or when it has
/// had no panes and no clients for `options.idle_shutdown` — removing its
/// socket and its lock on the way out either way.
///
/// # Errors
///
/// [`DaemonError::Lock`] naming the holder when another daemon has the lock,
/// and the binding and mirror failures of the rest.
///
/// # Panics
///
/// It spawns, so it must be called from within a Tokio runtime.
pub async fn serve(
    paths: &RuntimePaths,
    options: DaemonOptions,
    shutdown: watch::Receiver<bool>,
) -> Result<(), DaemonError> {
    let held = Lock::acquire(&paths.lock)?;
    let listener = socket::bind(&paths.socket)?;
    let registry = Arc::new(RwLock::new(Registry::new(
        RegistryDefaults {
            program: options.program.clone(),
            terminfo_directory: None,
        },
        Arc::new(Blocking::new(HistoryBudget::new(
            DEFAULT_HISTORY_BUDGET_BYTES,
        ))),
        MirrorThread::start()?,
    )));
    tracing::info!(socket = %paths.socket.display(), "the daemon is listening");
    let outcome = accept_until(&listener, &registry, &options, shutdown).await;
    let _removed = std::fs::remove_file(&paths.socket);
    held.release();
    outcome
}

/// The accept loop itself, until something says to stop: `shutdown` flipping,
/// `SIGTERM`, or having had no panes and no clients for the idle interval.
///
/// # Errors
///
/// [`DaemonError::Io`] when the signal handler cannot be installed.
///
/// # Panics
///
/// It spawns, so it must be called from within a Tokio runtime.
async fn accept_until(
    listener: &tokio::net::UnixListener,
    registry: &Arc<RwLock<Registry>>,
    options: &DaemonOptions,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), DaemonError> {
    let signal = registry.read().await.signal();
    let attached = Attached::default();
    let mut idling = Idle::new();
    let mut terminated = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|source| DaemonError::Io { source })?;
    // Held outside the loop: a deadline built inside the `select!` is rearmed
    // by every accept and every pane that speaks, so a busy daemon would never
    // look at whether it is idle and would then judge its first quiet second
    // against the time it started.
    let mut ticker = tokio::time::interval(IDLE_CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Once every sender is gone nothing can ask this daemon to stop, and a
    // `changed` that returns at once for ever would spin.
    let mut asked = true;
    loop {
        let due = tokio::select! {
            accepted = listener.accept() => {
                admit(accepted, registry, &attached).await;
                false
            }
            changed = shutdown.changed(), if asked => {
                if told(&changed, &mut shutdown, &mut asked) {
                    break;
                }
                false
            }
            _signalled = terminated.recv() => break,
            () = signal.notified() => {
                registry.write().await.ingest();
                false
            }
            _tick = ticker.tick() => true,
        };
        if due && spent(registry, &attached, &mut idling, options.idle_shutdown).await {
            tracing::info!("the daemon has had nothing to hold; going");
            break;
        }
    }

    Ok(())
}

/// The exit status of a command that could not do what it was asked.
const FAILED: u8 = 1;

/// Writes a line to standard error. The server never blocks on the standard
/// library's streams, so even a message goes through the runtime.
async fn complain(line: &str) {
    let mut stderr = tokio::io::stderr();
    let _written = stderr.write_all(format!("{line}\n").as_bytes()).await;
    let _flushed = stderr.flush().await;
}

/// Writes a line to standard output, the same way.
async fn say(line: &str) {
    let mut stdout = tokio::io::stdout();
    let _written = stdout.write_all(format!("{line}\n").as_bytes()).await;
    let _flushed = stdout.flush().await;
}

impl DaemonOptions {
    /// The options a command line asks for, or the flag it could not parse.
    /// The subcommand comes first and is not a flag.
    ///
    /// # Errors
    ///
    /// The offending argument, as words for a person.
    pub fn parse(arguments: &[OsString]) -> Result<DaemonOptions, String> {
        options_from(arguments)
    }
}

/// The options a command line asks for, or the flag it could not parse.
///
/// # Errors
///
/// The offending argument, as words for a person.
fn options_from(arguments: &[OsString]) -> Result<DaemonOptions, String> {
    let mut options = DaemonOptions::default();
    let mut rest = arguments.iter().skip(1);
    while let Some(argument) = rest.next() {
        let named = argument.to_string_lossy().into_owned();
        if named != IDLE_FLAG {
            return Err(format!("{named}: unknown flag"));
        }
        let seconds = rest
            .next()
            .ok_or_else(|| format!("{IDLE_FLAG}: a number of seconds must follow"))?;
        let parsed: u64 = seconds
            .to_string_lossy()
            .parse()
            .map_err(|_unparsed| format!("{IDLE_FLAG}: not a number of seconds"))?;
        options.idle_shutdown = Duration::from_secs(parsed);
    }
    Ok(options)
}

/// `--version`: the crate's version and the protocol's, on one line, because
/// the bootstrap of plan 0005 parses it.
async fn version() -> ExitCode {
    say(&format!(
        "iznik-server {SERVER_VERSION} protocol {PROTOCOL_VERSION}"
    ))
    .await;
    ExitCode::SUCCESS
}

/// `--foreground`: the accept loop in this process.
///
/// It leaves its parent's session first, so that the SSH session which
/// launched it cannot hang it up when it ends. That is what `--daemon`'s child
/// needs and what it always gets: `--daemon` spawns it in its own process
/// group, and a process that is not a group leader can always start a session.
///
/// Run **directly** from an interactive shell it is a different matter: job
/// control has already made it a group leader, `setsid` refuses, and it stays
/// in that shell's session and is hung up with it. `--foreground` is for a
/// daemon in the foreground, not for detaching; `--daemon` is the detaching
/// command, and the one the relay and the bootstrap use.
async fn foreground(arguments: &[OsString]) -> ExitCode {
    let options = match options_from(arguments) {
        Ok(options) => options,
        Err(refusal) => {
            complain(&refusal).await;
            return ExitCode::from(crate::USAGE_EXIT_CODE);
        }
    };
    if let Err(error) = setsid() {
        tracing::debug!(%error, "this process kept its caller's session");
    }
    let paths = match RuntimePaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            complain(&error.to_string()).await;
            return ExitCode::from(FAILED);
        }
    };
    let _logging = logging::initialize(&paths.log).await;
    let (_asked, shutdown) = watch::channel(false);
    match serve(&paths, options, shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Both: the log is where a daemon started with its streams on
            // `/dev/null` can be heard at all.
            tracing::error!(%error, "the daemon could not run");
            complain(&error.to_string()).await;
            ExitCode::from(FAILED)
        }
    }
}

/// How a wait for a daemon to answer ended.
#[derive(Debug)]
enum Ready {
    /// Its socket answered.
    Answered,
    /// The child it was waiting for ended first.
    Ended {
        /// What it exited with.
        status: std::process::ExitStatus,
    },
    /// The cap ran out with the child still there and still silent.
    Silent,
}

/// Waits for the socket to answer, up to `cap`, watching the child so that one
/// which has already gone is not waited out.
async fn answers_within(socket: &Path, child: &mut tokio::process::Child, cap: Duration) -> Ready {
    let started = std::time::Instant::now();
    while started.elapsed() < cap {
        if socket::answering(socket).await {
            return Ready::Answered;
        }
        if let Ok(Some(status)) = child.try_wait() {
            // It may have answered on its way out; the socket has the last word.
            if socket::answering(socket).await {
                return Ready::Answered;
            }
            return Ready::Ended { status };
        }
        tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
    }
    if socket::answering(socket).await {
        Ready::Answered
    } else {
        Ready::Silent
    }
}

/// `--daemon`: start one if there is not one already, and return as soon as
/// its socket answers.
///
/// Starting when one is already running is not an error — it is what the relay
/// does on every connection — so it says who holds it and succeeds.
async fn start(arguments: &[OsString]) -> ExitCode {
    let options = match options_from(arguments) {
        Ok(options) => options,
        Err(refusal) => {
            complain(&refusal).await;
            return ExitCode::from(crate::USAGE_EXIT_CODE);
        }
    };
    let paths = match RuntimePaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            complain(&error.to_string()).await;
            return ExitCode::from(FAILED);
        }
    };
    if socket::answering(&paths.socket).await {
        let holder = lock::holder(&paths.lock).unwrap_or_default();
        complain(&format!(
            "iznik-server is already running (process {holder})"
        ))
        .await;
        return ExitCode::SUCCESS;
    }
    let Ok(program) = std::env::current_exe() else {
        complain("this binary's own path could not be found").await;
        return ExitCode::from(FAILED);
    };
    // In the parent's process group: a process that is already a group leader
    // cannot start a session of its own, and the child must.
    let spawned = tokio::process::Command::new(program)
        .arg(FOREGROUND)
        .args(arguments.iter().skip(1))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            complain(&format!("the daemon could not be started: {error}")).await;
            return ExitCode::from(FAILED);
        }
    };
    match answers_within(&paths.socket, &mut child, options.socket_ready_cap).await {
        Ready::Answered => ExitCode::SUCCESS,
        // Waiting out the cap for a child that has already gone throws away
        // the one thing that says why; its own words are in the log.
        Ready::Ended { status } => {
            complain(&format!(
                "the daemon exited with {status} before answering on {}; see {}",
                paths.socket.display(),
                paths.log.display()
            ))
            .await;
            ExitCode::from(FAILED)
        }
        Ready::Silent => {
            complain(&format!(
                "the daemon did not answer on {} within {:?}",
                paths.socket.display(),
                options.socket_ready_cap
            ))
            .await;
            ExitCode::from(FAILED)
        }
    }
}

/// `--stop`: end a running daemon and wait for its socket to go.
async fn stop(arguments: &[OsString]) -> ExitCode {
    let options = match options_from(arguments) {
        Ok(options) => options,
        Err(refusal) => {
            complain(&refusal).await;
            return ExitCode::from(crate::USAGE_EXIT_CODE);
        }
    };
    let paths = match RuntimePaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            complain(&error.to_string()).await;
            return ExitCode::from(FAILED);
        }
    };
    // The lock is the truth and the recorded id is a courtesy: a daemon that
    // was killed leaves the file behind with an id the system will hand to
    // somebody else, and signalling that would be signalling a stranger.
    let holder = match Lock::acquire(&paths.lock) {
        Ok(nobody) => {
            nobody.release();
            complain("no iznik-server is running here").await;
            return ExitCode::from(FAILED);
        }
        Err(LockError::Held { process_id }) => process_id,
        Err(error) => {
            complain(&error.to_string()).await;
            return ExitCode::from(FAILED);
        }
    };
    if holder == 0 {
        complain("something holds the lock and the file names no process").await;
        return ExitCode::from(FAILED);
    }
    let Ok(pid) = i32::try_from(holder) else {
        complain("the lock file names no process this system could have").await;
        return ExitCode::from(FAILED);
    };
    if let Err(error) = signal::kill(Pid::from_raw(pid), Signal::SIGTERM) {
        complain(&format!(
            "process {holder} could not be told to stop: {error}"
        ))
        .await;
        return ExitCode::from(FAILED);
    }
    // Both, and in this order: the daemon removes its socket and only then
    // releases its lock, so a `--stop` that returned on the socket alone would
    // let the next `--daemon` start a child that dies holding nothing.
    let started = std::time::Instant::now();
    while started.elapsed() < options.stop_cap {
        if !paths.socket.exists() && lock::held_by(&paths.lock).is_none() {
            return ExitCode::SUCCESS;
        }
        tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
    }
    complain(&format!(
        "process {holder} was told to stop and has not let go"
    ))
    .await;
    ExitCode::from(FAILED)
}

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and this parses its own flags.
pub async fn run(arguments: &[OsString]) -> ExitCode {
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some(VERSION) => version().await,
        Some(FOREGROUND) => foreground(arguments).await,
        Some(DAEMON) => start(arguments).await,
        Some(STOP) => stop(arguments).await,
        _unknown => {
            complain("usage: iznik-server <--daemon | --foreground | --stop | --version>").await;
            ExitCode::from(crate::USAGE_EXIT_CODE)
        }
    }
}
