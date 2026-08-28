//! The process that lives on the server, held to what a real host does to it:
//! a runtime directory a locked-down machine allows, a lock rather than a
//! socket file for single instance, a session of its own so a disconnect
//! cannot hang it up, an exit when it has nothing to hold, and a log that
//! cannot fill a disk.
//!
//! Every case runs the real binary, because the things being proven —
//! detaching, locking, exiting — are properties of a process and not of a
//! function. Every idle interval is one second, so nothing here waits.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_server::daemon::DaemonOptions;
use iznik_server::daemon::idle::IDLE_SHUTDOWN;
use iznik_server::daemon::logging::{MAXIMUM_LOG_BYTES, Sink, predecessor};
use iznik_testkit::client::TestClient;
use iznik_testkit::pty::PtyChild;
use tokio::process::Command;

/// The binary every case runs.
const SERVER: &str = env!("CARGO_BIN_EXE_iznik-server");

/// The idle interval every case that watches a daemon go is given.
const BRISK: &str = "1";

/// The idle interval a case gives a daemon it expects to stay.
const PATIENT: &str = "3";

/// How long that daemon is kept busy, so a daemon measuring from when it
/// started would already be past its interval when the client leaves.
const BUSY_FOR: Duration = Duration::from_secs(2);

/// How long after the client leaves the daemon is looked at, which is well
/// inside [`PATIENT`] and well past a tick.
const SETTLE: Duration = Duration::from_millis(1500);

/// How long the probing case keeps connecting, which is longer than
/// [`PATIENT`].
const PROBING_FOR: Duration = Duration::from_millis(3500);

/// How long between those connections, which is longer than the interval the
/// daemon looks at its idleness on.
const PROBE_INTERVAL: Duration = Duration::from_millis(1200);

/// How long a case waits for something a daemon should do at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long a case waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long one line the log case writes is.
const LOG_LINE_BYTES: usize = 1024;

/// The size the pseudoterminal case runs its shell at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// How long the pseudoterminal case waits for a shell to fall quiet.
const QUIET: Duration = Duration::from_millis(300);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A runtime directory of a case's own, removed when it is dropped along with
/// whatever daemon is still holding it.
#[derive(Debug)]
struct Home {
    /// Where it is.
    path: PathBuf,
}

impl Home {
    /// A directory named for the case that asked for it.
    ///
    /// # Errors
    ///
    /// When it cannot be created.
    fn new(named: &str) -> Result<Home, Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let path = base.join(format!("iznik-daemon-{named}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Home { path })
    }

    /// The runtime directory the daemon will make under it.
    fn directory(&self) -> PathBuf {
        self.path.join("iznik")
    }

    /// The socket the daemon will listen on.
    fn socket(&self) -> PathBuf {
        self.directory().join("server.sock")
    }

    /// The lock file it will hold.
    fn lock(&self) -> PathBuf {
        self.directory().join("server.lock")
    }

    /// The log it will write.
    fn log(&self) -> PathBuf {
        self.directory().join("server.log")
    }

    /// The server, with this directory as its runtime base.
    fn server(&self) -> Command {
        let mut command = Command::new(SERVER);
        command
            .env("XDG_RUNTIME_DIR", &self.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Whatever is still holding it is told to go before the directory does.
        if let iznik_server::daemon::lock::Holder::Held { process_id } =
            iznik_server::daemon::lock::held_by(&self.lock())
            && let Ok(pid) = i32::try_from(process_id)
        {
            let _told = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// Waits until `looking` says so, or gives up.
///
/// # Errors
///
/// When it never does within [`PROMPT`].
async fn until(what: &str, mut looking: impl FnMut() -> bool) -> Result<(), Failed> {
    let started = Instant::now();
    while started.elapsed() < PROMPT {
        if looking() {
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Err(format!("{what} never happened within {PROMPT:?}").into())
}

/// Whether something is listening on a socket.
async fn answering(socket: &Path) -> bool {
    tokio::net::UnixStream::connect(socket).await.is_ok()
}

/// Starts a detached daemon under `home` and waits for it to answer.
///
/// # Errors
///
/// When the start fails or the socket never answers.
async fn start(home: &Home, idle: &str) -> Result<(), Failed> {
    let started = home
        .server()
        .arg("--daemon")
        .arg("--idle-shutdown-seconds")
        .arg(idle)
        .status()
        .await?;
    if !started.success() {
        return Err(format!("--daemon exited with {started}").into());
    }
    let socket = home.socket();
    if !answering(&socket).await {
        return Err(format!("nothing answers on {}", socket.display()).into());
    }
    Ok(())
}

/// The mode a directory carries, without the file-type bits.
///
/// # Errors
///
/// When it cannot be looked at.
fn mode_of(path: &Path) -> Result<u32, Failed> {
    let held = std::fs::metadata(path)?;
    let mode = <std::fs::Metadata as std::os::unix::fs::MetadataExt>::mode(&held);
    Ok(mode & 0o777)
}

/// # Panics
///
/// When the runtime directory is not where the host allows, or is readable by
/// anyone but its owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_runtime_directory_is_where_the_host_allows() {
    let case = async {
        let home = Home::new("paths")?;
        start(&home, BRISK).await?;
        assert!(
            home.socket().exists(),
            "with XDG_RUNTIME_DIR set the socket is under it"
        );
        assert_eq!(mode_of(&home.directory())?, 0o700, "and nobody else's");

        // Without it, under TMPDIR, in a directory of this user's own.
        let elsewhere = Home::new("paths-tmp")?;
        let mine = elsewhere
            .path
            .join(format!("iznik-{}", nix::unistd::Uid::current().as_raw()));
        let started = Command::new(SERVER)
            .env_remove("XDG_RUNTIME_DIR")
            .env("TMPDIR", &elsewhere.path)
            .arg("--daemon")
            .arg("--idle-shutdown-seconds")
            .arg(BRISK)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        assert!(started.success(), "it starts where TMPDIR allows");
        assert!(
            mine.join("server.sock").exists(),
            "and its socket is under TMPDIR: {}",
            mine.display()
        );
        let holder = iznik_server::daemon::lock::holder(&mine.join("server.lock"));
        if let Some(held) = holder
            && let Ok(pid) = i32::try_from(held)
        {
            let _told = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a second daemon runs against the same paths, or does not name the one
/// that already holds them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_one_daemon_holds_the_lock() {
    let case = async {
        let home = Home::new("single")?;
        let mut first = home.server().arg("--foreground").spawn()?;
        let held = first.id().ok_or("the first daemon has no process id")?;
        let socket = home.socket();
        until("the first daemon answers", || socket.exists()).await?;

        let second = home.server().arg("--foreground").output().await?;
        assert!(!second.status.success(), "the second refuses to run");
        let said = String::from_utf8_lossy(&second.stderr).into_owned();
        assert!(
            said.contains(&format!("process {held}")),
            "and names the one that holds it: {said}"
        );

        let _told = first.start_kill();
        let _waited = first.wait().await;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a dead predecessor's socket file stops a daemon starting, or a live
/// daemon's socket does not stop a second one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_socket_is_replaced_and_a_live_one_is_respected() {
    let case = async {
        let home = Home::new("hygiene")?;
        std::fs::create_dir_all(home.directory())?;
        std::fs::write(home.socket(), b"what a killed daemon left")?;
        start(&home, BRISK).await?;
        assert!(
            answering(&home.socket()).await,
            "a stale socket file is replaced, not respected"
        );

        let second = home.server().arg("--foreground").output().await?;
        assert!(
            !second.status.success(),
            "a live daemon's socket is left alone and the second start fails"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the daemon shares its parent's session, or does not outlive the shell
/// and the pseudoterminal it was started from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_outlives_the_shell_that_started_it() {
    let case = async {
        let home = Home::new("detach")?;
        let mut shell = PtyChild::spawn("sh", &[], COLUMNS, ROWS)?;
        let line = format!(
            "XDG_RUNTIME_DIR={} {SERVER} --daemon --idle-shutdown-seconds 30; echo started\n",
            home.path.display()
        );
        shell.write(line.as_bytes())?;
        let _said = shell.read_until_quiet(QUIET, PROMPT)?;
        let socket = home.socket();
        until("the daemon answers", || socket.exists()).await?;

        let holder =
            iznik_server::daemon::lock::holder(&home.lock()).ok_or("the lock names no holder")?;
        let daemon = nix::unistd::Pid::from_raw(i32::try_from(holder)?);
        let theirs = nix::unistd::getsid(Some(daemon))?;
        let ours = nix::unistd::getsid(None)?;
        assert_ne!(theirs, ours, "the daemon has a session of its own");

        // The shell goes, and the pseudoterminal with it: a hangup no daemon
        // that has left its session can be reached by.
        drop(shell);
        tokio::time::sleep(POLL_INTERVAL).await;
        assert!(
            answering(&socket).await,
            "and outlives the terminal it was started from"
        );
        assert!(
            nix::sys::signal::kill(daemon, None).is_ok(),
            "and is still a process"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a daemon with nothing to hold stays, or when the default interval is
/// not the one the module names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_daemon_goes() {
    let case = async {
        let asked = DaemonOptions::parse(&[OsString::from("--foreground")])?;
        assert_eq!(
            asked.idle_shutdown, IDLE_SHUTDOWN,
            "the default is the module's own"
        );

        let quiet = Home::new("idle")?;
        start(&quiet, BRISK).await?;
        let socket = quiet.socket();
        let lock = quiet.lock();
        until("the idle daemon goes", || !socket.exists()).await?;
        assert!(!lock.exists(), "and takes its lock with it");
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a daemon that has been busy for longer than its idle interval goes
/// the moment its last client leaves — which is what measuring idleness from
/// when it started rather than from when it last had something looks like.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_daemon_measures_idleness_from_when_it_last_had_something() {
    let case = async {
        let busy = Home::new("busy")?;
        start(&busy, PATIENT).await?;
        {
            let mut client = TestClient::connect(&busy.socket()).await?;
            let _greeting = client.hello(Capabilities::from_bits(0)).await?;
            let made = client
                .command(SessionCommand::CreateSession {
                    name: "work".to_owned(),
                    columns: COLUMNS,
                    rows: ROWS,
                    working_directory: None,
                })
                .await?;
            assert!(
                matches!(made, CommandOutcome::Applied { .. }),
                "the pane was made: {made:?}"
            );
            tokio::time::sleep(BUSY_FOR).await;
        }
        tokio::time::sleep(SETTLE).await;
        assert!(
            answering(&busy.socket()).await,
            "a daemon holding a pane stays, with or without a client"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When `--stop` signals whatever a stale lock file names rather than whoever
/// holds the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_believes_the_lock_and_not_the_file() {
    let case = async {
        let home = Home::new("stale-lock")?;
        std::fs::create_dir_all(home.directory())?;
        // What a killed daemon leaves: a lock file naming a process that is no
        // longer there, and an id the system will hand to somebody else.
        std::fs::write(
            home.lock(),
            b"4194303
",
        )?;
        let stopped = home.server().arg("--stop").output().await?;
        assert!(
            !stopped.status.success(),
            "it does not claim to have stopped one"
        );
        let said = String::from_utf8_lossy(&stopped.stderr).into_owned();
        assert!(
            said.contains("no iznik-server is running"),
            "and says nothing is running rather than signalling a stranger: {said}"
        );

        // And it leaves nothing behind: a `--stop` that took the lock to look
        // at it would have written its own process id into the file, which the
        // next one would read and signal.
        let empty = Home::new("no-lock")?;
        std::fs::create_dir_all(empty.directory())?;
        let looked = empty.server().arg("--stop").output().await?;
        assert!(!looked.status.success(), "with no lock file it fails too");
        assert!(
            !empty.lock().exists(),
            "and makes no lock file for the next one to believe"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a daemon that is touched between two looks at its idleness counts as
/// having been idle for the whole time between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_touched_between_looks_is_not_idle() {
    let case = async {
        let home = Home::new("probed")?;
        start(&home, PATIENT).await?;
        let socket = home.socket();
        // Brief connections and nothing else: no pane, no client that stays.
        // A count sampled once a second would see none of them.
        let started = Instant::now();
        while started.elapsed() < PROBING_FOR {
            // Dropped at once: a binding that lived across the sleep would
            // keep the client count above zero and prove nothing about a
            // connection that comes and goes between two looks.
            drop(tokio::net::UnixStream::connect(&socket).await);
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
        assert!(
            answering(&socket).await,
            "a daemon something keeps connecting to has not been idle"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a log past its cap is not rotated, or the two together are more than
/// twice the cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_log_is_capped_and_rotated() {
    let case = async {
        let home = Home::new("logs")?;
        std::fs::create_dir_all(home.directory())?;
        let path = home.log();
        let mut sink = Sink::open(&path, MAXIMUM_LOG_BYTES).await?;
        let line: String = core::iter::repeat_n('x', LOG_LINE_BYTES).collect();
        let mut written = 0_u64;
        while written < MAXIMUM_LOG_BYTES.saturating_add(MAXIMUM_LOG_BYTES / 4) {
            sink.write(&line).await?;
            written = written.saturating_add(u64::try_from(line.len())?);
        }

        let older = predecessor(&path);
        assert!(older.exists(), "the log past its cap is rotated");
        let total = std::fs::metadata(&path)?
            .len()
            .saturating_add(std::fs::metadata(&older)?.len());
        assert!(
            total < MAXIMUM_LOG_BYTES.saturating_mul(2),
            "and the two together are under twice the cap: {total}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When `--version` prints anything but the one line the bootstrap parses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_prints_one_line() {
    let case = async {
        let printed = Command::new(SERVER)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        assert!(printed.status.success(), "--version succeeds");
        assert!(printed.stderr.is_empty(), "and says nothing on stderr");
        let said = String::from_utf8(printed.stdout)?;
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one line: {said:?}");
        let line = lines.first().copied().unwrap_or_default();
        assert!(
            line.starts_with("iznik-server ") && line.contains(" protocol "),
            "with the crate version and the protocol version: {line}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When `--stop` does not end a running daemon, or does not say so when there
/// is none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_ends_a_daemon_and_says_when_there_is_none() {
    let case = async {
        let home = Home::new("stop")?;
        let missing = home.server().arg("--stop").output().await?;
        assert!(
            !missing.status.success(),
            "with no daemon it fails rather than pretending"
        );

        start(&home, "300").await?;
        let stopped = home.server().arg("--stop").output().await?;
        let said = String::from_utf8_lossy(&stopped.stderr).into_owned();
        assert!(stopped.status.success(), "it ends a running daemon: {said}");
        assert!(!home.socket().exists(), "and its socket goes");
        let lock = home.lock();
        until("the lock goes", || !lock.exists()).await?;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
