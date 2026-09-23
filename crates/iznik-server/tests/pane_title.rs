//! A title a pane already has is not a change.
//!
//! Plenty of prompts set the terminal's title on every line; every one of
//! those would otherwise be a delta sent to every client.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::delta::Delta;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The deadline the case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long it waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How many looks it takes before giving up.
const POLL_ATTEMPTS: usize = 1000;

/// What the pane prints last, so the case knows every title before it has
/// been read.
const DONE: &[u8] = b"announced-done";

/// # Panics
///
/// When a title set three times over is announced more than once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_title_an_unchanged_title_is_not_announced_again() {
    let case = async {
        let mut registry = Registry::new(
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
        let mut deltas = registry.deltas();
        let _session = registry
            .create_session("work".to_owned(), 80, 24, None)
            .await?;
        let pane = registry
            .snapshot()
            .sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .map(|held| held.id)
            .next()
            .ok_or("the session has a pane")?;
        let held = registry.pane(pane).cloned().ok_or("the pane")?;
        held.input(
            b"for n in 1 2 3; do printf '\\033]0;same\\007'; sleep 0.05; done; echo announced-done\n"
                .to_vec(),
        )?;
        let mut announced = 0_usize;
        for _look in 0..POLL_ATTEMPTS {
            registry.ingest();
            while let Ok(numbered) = deltas.try_recv() {
                if matches!(numbered.value, Delta::PaneTitle { .. }) {
                    announced = announced.saturating_add(1);
                }
            }
            let history = held.read_history(iznik_protocol::identity::Sequence(0))?;
            let finished = history
                .windows(DONE.len())
                .filter(|window| *window == DONE)
                .count()
                > 1;
            if finished {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        registry.ingest();
        while let Ok(numbered) = deltas.try_recv() {
            if matches!(numbered.value, Delta::PaneTitle { .. }) {
                announced = announced.saturating_add(1);
            }
        }
        assert_eq!(
            announced, 1,
            "the title changed once, and is announced once"
        );
        let _closed = registry.close_pane(pane);
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .unwrap_or_else(|_late| Err("the case ran past its deadline".into()))
        .unwrap_or_else(|error| panic!("{error}"));
}
