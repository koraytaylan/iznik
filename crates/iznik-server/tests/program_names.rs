//! A pane's model title becomes the foreground program, and its directory
//! becomes that process's directory, without the shell having to say so.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::identity::PaneId;
use iznik_protocol::model::HostModel;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::{MirrorError, MirrorThread};

/// How often the case samples. Short, so the assertion is not waiting on the
/// daemon's second.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(50);

/// How long a sample may take to reach the model.
const SAMPLE_DEADLINE: Duration = Duration::from_secs(3);

/// How often the case looks at the model while it waits.
const LOOK_INTERVAL: Duration = Duration::from_millis(20);

/// A registry that samples `program` on [`SAMPLE_INTERVAL`].
///
/// # Errors
///
/// When the mirror thread cannot start.
fn sample_registry(program: Program) -> Result<Registry, MirrorError> {
    Ok(Registry::new(
        RegistryDefaults {
            program,
            terminfo_directory: None,
            agent_socket: None,
            program_interval: SAMPLE_INTERVAL,
        },
        Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES))),
        MirrorThread::start()?,
    ))
}

/// The only pane's title and directory.
fn pane_text(model: &HostModel) -> Option<(PaneId, String, Option<String>)> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .next()
        .map(|pane| (pane.id, pane.title.clone(), pane.working_directory.clone()))
}

/// A foreground program names the pane, and the process's directory is recorded.
///
/// # Panics
///
/// When the sample never arrives, or names something other than the program.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_program_names_its_pane() {
    let mut registry = sample_registry(Program::Command {
        path: "sleep".into(),
        arguments: vec!["30".into()],
    })
    .expect("the registry");
    let _attached = registry.attach_client();
    registry
        .create_session("work".to_owned(), 80, 24, None)
        .await
        .expect("the session");
    let (pane, title, directory) = wait_for(&mut registry, |title, directory| {
        title == "sleep" && directory.is_some()
    })
    .await
    .expect("the sample");
    assert_eq!(title, "sleep");
    assert!(directory.is_some());
    registry.close_pane(pane).expect("the pane closes");
}

/// A shell does not name the tab after itself; the directory does.
///
/// # Panics
///
/// When the directory never arrives, or the title becomes the shell's name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_pane_has_its_directory() {
    let mut registry = sample_registry(Program::Command {
        path: "sh".into(),
        arguments: vec!["-c".into(), "sleep 30".into()],
    })
    .expect("the registry");
    let _attached = registry.attach_client();
    registry
        .create_session("work".to_owned(), 80, 24, None)
        .await
        .expect("the session");
    let (pane, title, directory) = wait_for(&mut registry, |title, directory| {
        title.is_empty() && directory.is_some()
    })
    .await
    .expect("the sample");
    assert!(
        title.is_empty(),
        "the shell is not the tab name, it was {title}"
    );
    assert!(directory.is_some());
    registry.close_pane(pane).expect("the pane closes");
}

/// Ingest until `ready`, or fail at [`SAMPLE_DEADLINE`].
///
/// # Errors
///
/// When no sample arrives in time, or the model holds no pane.
async fn wait_for(
    registry: &mut Registry,
    ready: impl Fn(&str, Option<&str>) -> bool,
) -> Result<(PaneId, String, Option<String>), String> {
    let deadline = tokio::time::Instant::now()
        .checked_add(SAMPLE_DEADLINE)
        .ok_or_else(|| "the deadline overflowed".to_owned())?;
    loop {
        registry.ingest();
        let (pane, title, directory) =
            pane_text(&registry.snapshot()).ok_or_else(|| "the model holds no pane".to_owned())?;
        if ready(&title, directory.as_deref()) {
            return Ok((pane, title, directory));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the sample did not arrive; title {title:?} directory {directory:?}"
            ));
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Nobody is shown a tab's name while no client is attached, so nothing is
/// sampled then; the first client to attach is shown it at once.
///
/// # Panics
///
/// When a pane is named with no client attached, or not named once one is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_read_with_nobody_attached() {
    let mut registry = sample_registry(Program::Command {
        path: "sleep".into(),
        arguments: vec!["30".into()],
    })
    .expect("the registry");
    registry
        .create_session("work".to_owned(), 80, 24, None)
        .await
        .expect("the session");
    tokio::time::sleep(SAMPLE_INTERVAL.saturating_mul(5)).await;
    registry.ingest();
    let (_pane, alone, _nowhere) = pane_text(&registry.snapshot()).expect("the pane");
    assert!(alone.is_empty(), "sampled with nobody attached: {alone}");
    let attached = registry.attach_client();
    let (pane, title, _directory) = wait_for(&mut registry, |named, _read| named == "sleep")
        .await
        .expect("the sample once a client attaches");
    assert_eq!(title, "sleep");
    drop(attached);
    registry.close_pane(pane).expect("the pane closes");
}
