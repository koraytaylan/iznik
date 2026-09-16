//! Headless proofs for model-driven tab and session bars.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Context, IntoElement, Render, TestAppContext, Window, WindowHandle};
use iznik_app::bars;
use iznik_app::host_ui::EngineState;
use iznik_app::window::TabKey;
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

#[test]
/// Keyboard navigation wraps through bar entries in either direction.
///
/// # Panics
///
/// Panics when navigation leaves the valid entry range.
fn bar_navigation_wraps() {
    assert_eq!(bars::next_index(0, 3, false), 2);
    assert_eq!(bars::next_index(2, 3, true), 0);
    assert_eq!(bars::next_index(0, 0, true), 0);
}

#[test]
/// Close affordances produce the protocol commands consumed by the shell.
///
/// # Panics
///
/// Panics when a close affordance does not build its expected command.
fn close_affordances_build_commands() {
    assert_eq!(
        bars::close_tab(TabId(3)),
        iznik_protocol::command::SessionCommand::CloseTab { tab: TabId(3) }
    );
    assert_eq!(
        bars::close_session(SessionId(2)),
        iznik_protocol::command::SessionCommand::CloseSession {
            session: SessionId(2)
        }
    );
}

#[test]
/// Tab switching follows the settled session order and wraps at both ends.
///
/// # Panics
///
/// Panics when model order or wrapping does not produce the expected tab.
fn tab_switching_wraps_model_order() {
    let mut state = EngineState::new();
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let model = two_tab_model();
    let payload = encode_host_model(&model).expect("two-tab model encodes");
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload,
        },
    );
    let first = TabKey {
        host,
        session: SessionId(2),
        tab: TabId(3),
    };
    assert_eq!(
        bars::next_tab(&state, Some(&first), true)
            .expect("next tab exists")
            .tab,
        TabId(5)
    );
    assert_eq!(
        bars::next_tab(&state, Some(&first), false)
            .expect("previous tab exists")
            .tab,
        TabId(5)
    );
}

#[test]
/// A tab removal delta removes the close target from the visible model order.
///
/// # Panics
///
/// Panics when the reducer leaves a removed tab in the bars' projection.
fn tab_removal_reconciles_bars() {
    let mut state = EngineState::new();
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let model = two_tab_model();
    let payload = encode_host_model(&model).expect("two-tab model encodes");
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload,
        },
    );
    let selected = TabKey {
        host: host.clone(),
        session: SessionId(2),
        tab: TabId(3),
    };
    let delta = encode_delta(&Delta::TabRemoved { tab: TabId(3) }).expect("tab delta encodes");
    state.apply(
        &host,
        &ToClient::Delta {
            generation: Generation(2),
            payload: delta,
        },
    );
    assert!(
        !bars::tab_keys(&state, Some(&selected))
            .iter()
            .any(|key| key.tab == TabId(3))
    );
}

/// A root that renders the bars from one settled engine state.
struct BarsFixture {
    /// The model state shown by the bars.
    state: EngineState,
}

impl Render for BarsFixture {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        bars::render(&self.state, None, None)
    }
}

/// Empty state is explicit in both strips.
#[gpui_kit::test]
fn bars_render_empty_states(context: &mut TestAppContext) {
    let result = empty_states(context);
    check(&result);
}

/// A settled host model produces one session and tab entry with stable ids.
#[gpui_kit::test]
fn bars_render_settled_model_entries(context: &mut TestAppContext) {
    let result = settled_entries(context);
    check(&result);
}

/// Assert a bars case without making the GPUI test macro own its error path.
///
/// # Panics
/// Fails with the underlying bars fixture error.
fn check(result: &Result<(), Box<dyn std::error::Error>>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Render the empty bars and inspect both explicit empty states.
///
/// # Errors
/// Returns a closed-window error.
fn empty_states(context: &mut TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    let handle = context.add_window(|_, _| BarsFixture {
        state: EngineState::new(),
    });
    draw(context, handle)?;
    context.update_window(handle.into(), |_, window, _| {
        window
            .find("tab-bar-empty")
            .visible()
            .then_some(())
            .ok_or("tab empty state is missing")?;
        window
            .find("session-bar-empty")
            .visible()
            .then_some(())
            .ok_or("session empty state is missing")?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })??;
    Ok(())
}

/// Render settled model entries and inspect their stable identifiers.
///
/// # Errors
/// Returns a model encoding or closed-window error.
fn settled_entries(context: &mut TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    let mut state = EngineState::new();
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(2),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(3),
                name: "editor".to_owned(),
                panes: vec![Pane {
                    id: PaneId(4),
                    title: "shell".to_owned(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(PaneId(4)),
            }],
        }],
    };
    let payload = encode_host_model(&model)?;
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload,
        },
    );
    let handle = context.add_window(|_, _| BarsFixture { state });
    draw(context, handle)?;
    context.update_window(handle.into(), |_, window, _| {
        window
            .find("session-build-2")
            .visible()
            .then_some(())
            .ok_or("session entry is missing")?;
        window
            .find("tab-build-3")
            .visible()
            .then_some(())
            .ok_or("tab entry is missing")?;
        window
            .find("tab-close-build-3")
            .visible()
            .then_some(())
            .ok_or("tab close affordance is missing")?;
        window
            .find("session-close-build-2")
            .visible()
            .then_some(())
            .ok_or("session close affordance is missing")?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })??;
    Ok(())
}

/// Construct a session with two tabs for deterministic switching assertions.
fn two_tab_model() -> HostModel {
    let pane = |pane_id: PaneId, title: &str| Pane {
        id: pane_id,
        title: title.to_owned(),
        working_directory: None,
        columns: 80,
        rows: 24,
    };
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(2),
            name: "work".to_owned(),
            tabs: vec![
                Tab {
                    id: TabId(3),
                    name: "editor".to_owned(),
                    panes: vec![pane(PaneId(4), "shell")],
                    layout: LayoutNode::Leaf(PaneId(4)),
                },
                Tab {
                    id: TabId(5),
                    name: "logs".to_owned(),
                    panes: vec![pane(PaneId(6), "logs")],
                    layout: LayoutNode::Leaf(PaneId(6)),
                },
            ],
        }],
    }
}

/// Draw a fixture and flush its element tree.
///
/// # Errors
/// Returns the closed-window error from GPUI.
fn draw(
    context: &mut TestAppContext,
    handle: WindowHandle<BarsFixture>,
) -> Result<(), Box<dyn std::error::Error>> {
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    Ok(())
}
