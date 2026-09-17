//! The flow from no host to a focused terminal: what the stage offers in each
//! host state, how a host's state reads, and what the window follows after a
//! person asks for something.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
use iznik_app::actions::ActionId;
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::follow::{Expectation, arrived_pane, arrived_tab, expectation, ready_to_start};
use iznik_app::host_ui::EngineState;
use iznik_app::stage::{Stage, stage};
use iznik_app::status::{Remedy, Tone, summary, troubled};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, TabKey, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::HostState;
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// A host by alias.
fn host(alias: &str) -> HostId {
    HostId(alias.to_owned())
}

/// A connected state with no upgrade on offer.
fn connected() -> HostState {
    HostState::Connected {
        server_version: "0.1.0".to_owned(),
        upgrade: None,
    }
}

/// A failed state.
fn failed() -> HostState {
    HostState::Failed {
        error: "ssh: Could not resolve hostname".to_owned(),
        retry_at: std::time::Instant::now(),
    }
}

/// Tell a state that a host is in a connection state.
fn moved(state: &mut EngineState, alias: &str, connection: HostState) {
    state.absorb(EngineEvent::Said(ManagerEvent::Moved {
        host: host(alias),
        state: connection,
    }));
}

/// A host model holding one session with the given tabs, each holding its
/// one given pane.
fn model(tabs: &[(u64, u64)]) -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "session".to_owned(),
            tabs: tabs
                .iter()
                .map(|(tab, pane)| Tab {
                    id: TabId(*tab),
                    name: format!("tab {tab}"),
                    panes: vec![Pane {
                        id: PaneId(*pane),
                        title: String::new(),
                        working_directory: None,
                        columns: 80,
                        rows: 24,
                    }],
                    layout: LayoutNode::Leaf(PaneId(*pane)),
                })
                .collect(),
        }],
    }
}

/// Give a state a host's model.
///
/// # Errors
///
/// Returns the encoding failure.
fn snapshot(state: &mut EngineState, alias: &str, model: &HostModel) -> Result<(), Failed> {
    state.apply(
        &host(alias),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(model)?,
        },
    );
    Ok(())
}

#[test]
/// With nothing held the stage welcomes; a host being reached, or one that
/// could not be, is described with its remedies; a connected one offers a session.
///
/// # Panics
///
/// Panics when a stage differs.
fn the_stage_follows_the_host_a_person_is_working_with() {
    let mut state = EngineState::new();
    assert_eq!(stage(&state, None), Stage::Welcome);
    moved(&mut state, "devbox", HostState::Probing);
    let Stage::Host {
        host: about,
        summary: reading,
    } = stage(&state, Some(&host("devbox")))
    else {
        panic!("a host being reached is described");
    };
    assert_eq!(about, host("devbox"));
    assert_eq!(reading.tone, Tone::Progress);
    assert_eq!(reading.remedies, [Remedy::Cancel]);
    moved(&mut state, "devbox", failed());
    moved(&mut state, "alpha", connected());
    let Stage::Host {
        summary: unreachable,
        ..
    } = stage(&state, Some(&host("devbox")))
    else {
        panic!("the preferred failed host is described");
    };
    assert_eq!(unreachable.headline, "Couldn\u{2019}t connect to devbox");
    assert_eq!(unreachable.remedies, [Remedy::Retry, Remedy::Remove]);
    assert_eq!(
        stage(&state, Some(&host("alpha"))),
        Stage::Empty {
            host: host("alpha")
        }
    );
    assert_eq!(
        stage(&state, Some(&host("forgotten"))),
        Stage::Empty {
            host: host("alpha")
        },
        "a preferred host no longer held falls back to the first held"
    );
}

#[test]
/// Every state reads with the host's name, and the strips skip connected
/// hosts and the host the stage already describes.
///
/// # Panics
///
/// Panics when a reading or the strip set differs.
fn host_states_read_with_the_host_name_and_banners_do_not_repeat_the_stage() {
    let reconnecting = summary(
        &host("devbox"),
        &HostState::Reconnecting {
            attempt: 3,
            trouble: Some("link went quiet".to_owned()),
            retry_at: std::time::Instant::now(),
        },
    );
    assert_eq!(reconnecting.headline, "Reconnecting to devbox\u{2026}");
    assert_eq!(
        reconnecting.detail.as_deref(),
        Some("Attempt 3: link went quiet")
    );
    assert_eq!(reconnecting.tone, Tone::Warning);
    assert_eq!(summary(&host("devbox"), &connected()).tone, Tone::Good);
    let mut state = EngineState::new();
    moved(&mut state, "alpha", connected());
    moved(&mut state, "broken", failed());
    moved(&mut state, "devbox", HostState::Connecting);
    let banners: Vec<_> = troubled(&state, Some(&host("devbox")))
        .into_iter()
        .map(|(alias, _)| alias.clone())
        .collect();
    assert_eq!(banners, [host("broken")]);
}

#[test]
/// A created tab or pane is recognised once it arrives, and not before.
///
/// # Panics
///
/// Panics when an expectation resolves wrongly.
fn created_tabs_and_panes_are_found_once_present() {
    let mut state = EngineState::new();
    snapshot(&mut state, "devbox", &model(&[(1, 10)])).expect("model encodes");
    let Some(Expectation::Tab { known, .. }) =
        expectation(ActionId::CreateTab, &state, &host("devbox"))
    else {
        panic!("creating a tab expects one");
    };
    assert_eq!(known, BTreeSet::from([TabId(1)]));
    assert_eq!(arrived_tab(&state, &host("devbox"), &known), None);
    let Some(Expectation::Pane { known: panes, .. }) =
        expectation(ActionId::CreatePane, &state, &host("devbox"))
    else {
        panic!("creating a pane expects one");
    };
    assert_eq!(
        expectation(ActionId::RenameTab, &state, &host("devbox")),
        None
    );
    snapshot(&mut state, "devbox", &model(&[(1, 10), (2, 20)])).expect("model encodes");
    assert_eq!(
        arrived_tab(&state, &host("devbox"), &known),
        Some(TabKey {
            host: host("devbox"),
            session: SessionId(1),
            tab: TabId(2),
        })
    );
    assert_eq!(
        arrived_pane(&state, &host("devbox"), &panes),
        Some(PaneId(20))
    );
}

#[test]
/// A host added from the window is ready for its first session only once it
/// is connected and its model has arrived.
///
/// # Panics
///
/// Panics when readiness differs.
fn an_added_host_starts_once_connected_with_its_model() {
    let mut state = EngineState::new();
    let starting = BTreeSet::from([host("devbox")]);
    moved(&mut state, "devbox", HostState::Connecting);
    assert!(ready_to_start(&state, &starting).is_empty());
    moved(&mut state, "devbox", connected());
    assert!(
        ready_to_start(&state, &starting).is_empty(),
        "no model yet: whether it holds sessions is unknown"
    );
    snapshot(
        &mut state,
        "devbox",
        &HostModel {
            generation: Generation(1),
            sessions: Vec::new(),
        },
    )
    .expect("model encodes");
    assert_eq!(ready_to_start(&state, &starting), [&host("devbox")]);
}

/// With no host the window's body is the welcome stage with its Add Host button.
#[gpui_kit::test]
fn the_empty_window_offers_add_host(context: &mut TestAppContext) {
    check(&welcome(context));
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
///
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Open a headless shell with no host and look at its body.
///
/// # Errors
///
/// Returns setup failures.
///
/// # Panics
///
/// Panics when the welcome stage or its button is missing.
fn welcome(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory: PathBuf =
        std::env::temp_dir().join(format!("iznik-app-flow-{}", std::process::id()));
    std::fs::remove_dir_all(&directory).ok();
    std::fs::create_dir_all(directory.join("artifacts"))?;
    let bridge = EngineBridge::start(
        directory.join("artifacts"),
        ClientRuntimePaths::under(&directory.join("runtime"))?,
    )?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let (_view, context) = context.add_window_view(|window, context| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ..ShellOptions::default()
            },
            window,
            context,
        )
    });
    context.update(|window, _application| {
        window.find("stage-welcome").visible();
        window.find("stage-add-host").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}
