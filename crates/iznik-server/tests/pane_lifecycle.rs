//! How a pane ends when it is closed, and when its shell ends on its own: the
//! lifecycle between the registry that forgets a pane and the process group
//! that pane stood for.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pane::Pane;
use iznik_server::pty::spawn::Program;
use iznik_server::pty::spawn::{ExitStatus, SpawnOptions};
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::{MirrorError, MirrorThread};

/// The size every pane in these cases is created at.
const COLUMNS: u16 = 80;

/// The height every pane in these cases is created at.
const ROWS: u16 = 24;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(10);

/// How often a case looks for a file the shell writes.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A directory of its own for one case, empty.
///
/// # Errors
///
/// When the directory cannot be made.
fn scratch(case: &str) -> std::io::Result<PathBuf> {
    let directory =
        std::env::temp_dir().join(format!("iznik-lifecycle-{case}-{}", std::process::id()));
    let _removed = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

/// Waits until `path` exists, within [`DEADLINE`].
async fn appears(path: &Path) -> bool {
    tokio::time::timeout(DEADLINE, async {
        while !path.exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .is_ok()
}

/// A registry whose panes run `script` under `sh`.
///
/// # Errors
///
/// When the mirror thread cannot be started.
fn registry_running(script: String) -> Result<Registry, MirrorError> {
    Ok(Registry::new(
        RegistryDefaults {
            program: Program::Command {
                path: "sh".into(),
                arguments: vec!["-c".to_owned(), script],
            },
            terminfo_directory: None,
            agent_socket: None,
            program_interval: Duration::ZERO,
        },
        Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES))),
        MirrorThread::start()?,
    ))
}

/// A pane closed through the registry is hung up and given its grace period:
/// the shell's `HUP` trap runs to the end — taking a moment first, as a shell
/// saving its history would — rather than being killed the instant the
/// registry lets go of the pane.
///
/// # Panics
///
/// When the trap never finishes because the pane was killed at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_lifecycle_a_closed_pane_gets_its_grace() {
    let directory = scratch("grace").expect("a scratch directory");
    let ready = directory.join("ready");
    let hung = directory.join("hung");
    let script = format!(
        "trap 'sleep 0.3; echo hung > \"{}\"; exit 0' HUP; : > \"{}\"; while :; do sleep 0.05; done",
        hung.display(),
        ready.display()
    );
    let mut registry = registry_running(script).expect("a registry");
    let _session = registry
        .create_session("work".to_owned(), COLUMNS, ROWS, None)
        .await
        .expect("a session");
    let pane = registry
        .snapshot()
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .map(|pane| pane.id)
        .next()
        .expect("the only pane");
    assert!(appears(&ready).await, "the shell set its trap");
    registry.close_pane(pane).expect("the pane closes");
    assert!(
        appears(&hung).await,
        "the hung-up shell finished its trap within the grace period"
    );
    let _removed = std::fs::remove_dir_all(&directory);
}

/// A daemon told to stop hangs every pane up and gives it the grace period,
/// the same as a pane closed on its own.
///
/// # Panics
///
/// When the trap never finishes because stopping killed the pane at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_lifecycle_closing_everything_hangs_up_first() {
    let directory = scratch("everything").expect("a scratch directory");
    let ready = directory.join("ready");
    let hung = directory.join("hung");
    let script = format!(
        "trap 'sleep 0.3; echo hung > \"{}\"; exit 0' HUP; : > \"{}\"; while :; do sleep 0.05; done",
        hung.display(),
        ready.display()
    );
    let mut registry = registry_running(script).expect("a registry");
    let _session = registry
        .create_session("work".to_owned(), COLUMNS, ROWS, None)
        .await
        .expect("a session");
    assert!(appears(&ready).await, "the shell set its trap");
    tokio::time::timeout(DEADLINE, registry.close_all())
        .await
        .expect("closing everything finishes");
    assert!(
        appears(&hung).await,
        "the hung-up shell finished its trap before it was killed"
    );
    let _removed = std::fs::remove_dir_all(&directory);
}

/// Closing a pane whose shell has already ended and been reaped signals
/// nothing: its process id belongs to nobody now, and the close succeeds
/// rather than failing on a group that is gone.
///
/// # Panics
///
/// When the close tries to hang up the reaped child and fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_lifecycle_closing_an_ended_pane_signals_nothing() {
    let thread = MirrorThread::start().expect("the mirror thread");
    let options = SpawnOptions {
        program: Program::Command {
            path: "sh".into(),
            arguments: vec!["-c".to_owned(), "exit 3".to_owned()],
        },
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
        terminfo_directory: None,
        agent_socket: None,
    };
    let pane = Pane::spawn(&options, DEFAULT_HISTORY_BUDGET_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let status = tokio::time::timeout(DEADLINE, pane.exit_status())
        .await
        .expect("the exit is reported in time");
    assert_eq!(
        status,
        Some(ExitStatus::Exited(3)),
        "the shell ended itself"
    );
    pane.close().expect("closing an ended pane is not an error");
}

/// The process id a shell wrote to `path`, within [`DEADLINE`].
///
/// # Errors
///
/// When no process id is in the file within [`DEADLINE`].
async fn written_process(path: &Path) -> Result<i32, Box<dyn std::error::Error>> {
    // The file appears before the shell's `echo` has filled it.
    let read = tokio::time::timeout(DEADLINE, async {
        loop {
            if let Ok(parsed) = std::fs::read_to_string(path)
                .unwrap_or_default()
                .trim()
                .parse()
            {
                return parsed;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await?;
    Ok(read)
}

/// Whether the process is still there, zombies included.
fn running(process: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(process), None).is_ok()
}

/// A pane whose shell runs `script`, at the usual size.
fn running_script(script: String) -> SpawnOptions {
    SpawnOptions {
        program: Program::Command {
            path: "sh".into(),
            arguments: vec!["-c".to_owned(), script],
        },
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
        terminfo_directory: None,
        agent_socket: None,
    }
}

/// A shell that exits leaving a background job on its terminal is reaped
/// and reported at once rather than when the job lets go of the terminal, and
/// the job left in its group is hung up.
///
/// # Panics
///
/// When the exit is never reported or the job outlives the shell.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_lifecycle_a_background_job_does_not_keep_a_pane_alive() {
    let directory = scratch("background").expect("a scratch directory");
    let job = directory.join("job");
    let script = format!("sleep 1000 & echo $! > \"{}\"; exit 0", job.display());
    let thread = MirrorThread::start().expect("the mirror thread");
    let pane = Pane::spawn(
        &running_script(script),
        DEFAULT_HISTORY_BUDGET_BYTES,
        &thread,
    )
    .await
    .expect("the pane spawns");
    let process = written_process(&job).await.expect("the job's process id");
    let status = tokio::time::timeout(DEADLINE, pane.exit_status())
        .await
        .expect("the exit is reported while the job could still be running");
    assert_eq!(
        status,
        Some(ExitStatus::Exited(0)),
        "the shell's own status"
    );
    let mut state = pane.state_updates();
    tokio::time::timeout(DEADLINE, state.wait_for(|state| state.exited))
        .await
        .expect("the pane ends in time")
        .expect("the pane says it has ended");
    let gone = tokio::time::timeout(DEADLINE, async {
        while running(process) {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    if gone.is_err() {
        let _killed = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    assert!(
        gone.is_ok(),
        "the job left in the shell's group was hung up"
    );
    let _removed = std::fs::remove_dir_all(&directory);
}

/// A background job that ignores the hangup and keeps the terminal open does
/// not keep its pane from ending either.
///
/// # Panics
///
/// When the pane waits for the job.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_lifecycle_a_job_ignoring_the_hangup_does_not_keep_a_pane_alive() {
    let directory = scratch("ignoring").expect("a scratch directory");
    let job = directory.join("job");
    let script = format!(
        "(trap '' HUP; exec sleep 1000) & echo $! > \"{}\"; exit 0",
        job.display()
    );
    let thread = MirrorThread::start().expect("the mirror thread");
    let pane = Pane::spawn(
        &running_script(script),
        DEFAULT_HISTORY_BUDGET_BYTES,
        &thread,
    )
    .await
    .expect("the pane spawns");
    let process = written_process(&job).await.expect("the job's process id");
    let mut state = pane.state_updates();
    let ended = tokio::time::timeout(DEADLINE, state.wait_for(|state| state.exited)).await;
    let _killed = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(process),
        nix::sys::signal::Signal::SIGKILL,
    );
    assert!(ended.is_ok(), "the pane ended with the job still running");
    assert_eq!(
        pane.exit_status_now(),
        Some(ExitStatus::Exited(0)),
        "and says how its shell ended"
    );
    let _removed = std::fs::remove_dir_all(&directory);
}

/// A pane whose spawn fails after its shell has started — here because its
/// mirror thread has ended — ends that shell rather than leaving it running
/// with nothing to answer to.
///
/// # Panics
///
/// When the spawn succeeds, or the shell outlives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_lifecycle_a_failed_spawn_leaves_no_shell_behind() {
    let directory = scratch("failed").expect("a scratch directory");
    let shell = directory.join("shell");
    let script = format!("echo $$ > \"{}\"; exec sleep 1000", shell.display());
    let mut thread = MirrorThread::start().expect("the mirror thread");
    thread.stop();
    let spawned = Pane::spawn(
        &running_script(script),
        DEFAULT_HISTORY_BUDGET_BYTES,
        &thread,
    )
    .await;
    assert!(spawned.is_err(), "a pane without its mirror does not spawn");
    // A shell killed before it wrote its number is one that did not survive.
    let written = tokio::time::timeout(Duration::from_secs(1), written_process(&shell)).await;
    let Ok(Ok(process)) = written else {
        let _removed = std::fs::remove_dir_all(&directory);
        return;
    };
    let gone = tokio::time::timeout(DEADLINE, async {
        while running(process) {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    if gone.is_err() {
        let _killed = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(process),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    assert!(gone.is_ok(), "the shell of the failed spawn was ended");
    let _removed = std::fs::remove_dir_all(&directory);
}
