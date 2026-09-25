//! An adopted pane's first screen is the emulator's own account of the ring
//! it carried, and a resume uses that ring the way a reconnect always has.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::identity::{Generation, PaneId, Sequence, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab};
use iznik_server::adopt::{ADOPTED_STATE_VERSION, AdoptedPane, AdoptedState, TermiosState};
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::{Program, SpawnOptions, spawn};
use iznik_server::resume::{StartPlan, StartRequest, plan_start};
use iznik_server::session::instance::fresh_instance;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::{Mirror, MirrorThread};
use iznik_server::terminal::screen::serialize;

/// The carried sequence, past the ring, so a byte before the ring is outside it.
const CARRIED: u64 = 1_000;

/// The ring, ending in the middle of an escape.
fn ring() -> Vec<u8> {
    b"hello\r\n\x1b[31".to_vec()
}

/// A model of one pane.
fn model(pane: PaneId) -> HostModel {
    HostModel {
        generation: Generation(3),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                panes: vec![Pane {
                    id: pane,
                    title: String::new(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(pane),
            }],
        }],
    }
}

/// What the emulator itself serializes for `ring` at `sequence`.
///
/// # Errors
///
/// When the mirror thread, the emulator, or the wait fails.
fn emulator_screen(ring: &[u8], sequence: Sequence) -> Result<Vec<u8>, String> {
    let mirrors = MirrorThread::start().map_err(|error| error.to_string())?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let bytes = ring.to_vec();
    mirrors.spawn(move || async move {
        let Ok(mut mirror) = Mirror::new(80, 24) else {
            return;
        };
        mirror.feed(&bytes);
        if let Ok(screen) = serialize(&mirror, sequence) {
            let _sent = sender.send(screen.bytes);
        }
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())
}

/// # Panics
///
/// When the adopted screen is not the emulator's, or a resume is not contiguous
/// inside the ring and a screen outside it.
#[tokio::test]
async fn an_adopted_pane_screen_is_the_emulators_and_resume_follows_the_ring() {
    let pane = PaneId(1);
    let ring = ring();
    let sequence = Sequence(CARRIED);
    let expected = emulator_screen(&ring, sequence).expect("the emulator serializes the ring");
    let spawned = spawn(&SpawnOptions {
        program: Program::Command {
            path: "sleep".into(),
            arguments: vec!["30".into()],
        },
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
        agent_socket: None,
    })
    .expect("a child is running");
    let descriptor = spawned.duplicate_master().expect("the master duplicates");
    let adopted = iznik_server::pty::spawn::PtyProcess::adopt(descriptor, spawned.process_id())
        .expect("the master is adopted");
    let mirrors = MirrorThread::start().expect("the registry's mirror thread starts");
    let registry = Registry::adopt(
        RegistryDefaults {
            program: Program::LoginShell,
            terminfo_directory: None,
            agent_socket: None,
            program_interval: Duration::ZERO,
        },
        Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES))),
        mirrors,
        &AdoptedState {
            version: ADOPTED_STATE_VERSION,
            instance: fresh_instance(),
            model: model(pane),
            panes: vec![AdoptedPane {
                pane,
                descriptor,
                process_id: spawned.process_id(),
                sequence,
                ring: ring.clone(),
                termios: TermiosState { bytes: Vec::new() },
            }],
        },
        std::collections::BTreeMap::from([(pane, adopted)]),
    )
    .await
    .expect("the registry adopts the pane");
    let held = registry.pane(pane).expect("the pane is held");
    let screen = held.screen().await.expect("the pane has a screen");
    assert_eq!(
        screen.sequence, sequence,
        "the screen is at the carried sequence"
    );
    assert_eq!(
        screen.bytes, expected,
        "and the bytes are the emulator's own serialization"
    );
    let view = held.state();
    let inside = plan_start(
        &StartRequest::Resume {
            pane,
            from: view.oldest,
        },
        view.oldest,
        view.newest,
    );
    assert!(
        matches!(inside, StartPlan::Continue { .. }),
        "a byte the ring covers resumes: {inside:?}"
    );
    let continued = held
        .read_history(view.oldest)
        .expect("the ring is readable");
    assert_eq!(continued, ring, "and the bytes are the ring, contiguous");
    let outside = plan_start(
        &StartRequest::Resume {
            pane,
            from: Sequence(0),
        },
        view.oldest,
        view.newest,
    );
    assert!(
        matches!(outside, StartPlan::Screen { .. }),
        "a byte the ring does not cover is a screen: {outside:?}"
    );
    drop(registry);
    drop(spawned);
}
