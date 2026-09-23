//! How a pane ends when it is closed, and when its shell ends on its own: the
//! lifecycle between the registry that forgets a pane and the process group
//! that pane stood for.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
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
