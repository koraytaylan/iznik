//! Headless proofs for model-driven tab and session bars.

use gpui_kit::component::ActiveTheme;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, IntoElement, ParentElement, Render, ScrollDelta, ScrollHandle, Styled,
    TestAppContext, Window, WindowHandle, div, point, px, size,
};
use iznik_app::bars;
use iznik_app::host_ui::EngineState;
use iznik_app::navigation::ShortcutHint;
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
/// Closing a tab in a later session stays with that session's neighbor tab.
///
/// Three sessions of three tabs is the case that used to jump to the first
/// tab of the first session.
///
/// # Panics
///
/// Panics when the neighbor is the first session, or when a closed session
/// does not yield the session beside it.
fn closing_a_tab_stays_in_its_session() {
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let mut state = settled_three_sessions(&host).expect("three sessions encode");
    let mut place = bars::tab_place(&state, &tab_key(SessionId(2), TabId(5)))
        .expect("the middle tab is present");
    apply_tab_removed(&mut state, &host, TabId(5), Generation(2)).expect("tab removal encodes");
    let next = bars::tab_after_close(&state, &place).expect("the session keeps a tab");
    assert_eq!(next.session, SessionId(2), "session 2 stays on screen");
    assert_eq!(next.tab, TabId(6), "the tab that followed the closed one");

    place = bars::tab_place(&state, &tab_key(SessionId(2), TabId(6)))
        .expect("the following tab is present");
    apply_tab_removed(&mut state, &host, TabId(6), Generation(3)).expect("tab removal encodes");
    let previous = bars::tab_after_close(&state, &place).expect("a tab remains before it");
    assert_eq!(
        previous.session,
        SessionId(2),
        "the same session stays on screen"
    );
    assert_eq!(previous.tab, TabId(4), "the tab before the closed one");

    place = bars::tab_place(&state, &tab_key(SessionId(2), TabId(4)))
        .expect("the last tab of session 2 is present");
    apply_tab_removed(&mut state, &host, TabId(4), Generation(4)).expect("tab removal encodes");
    let moved = bars::tab_after_close(&state, &place).expect("session 3 follows session 2");
    assert_eq!(moved.session, SessionId(3), "the next session opens");
    assert_eq!(moved.tab, TabId(7), "that session's first tab");

    place = bars::tab_place(&state, &tab_key(SessionId(3), TabId(7)))
        .expect("session 3's first tab is present");
    apply_tab_removed(&mut state, &host, TabId(7), Generation(5)).expect("tab removal encodes");
    let shown = bars::tab_after_close(&state, &place).expect("the next tab of session 3");
    assert_eq!(shown.session, SessionId(3), "session 3 stays on screen");
    assert_eq!(shown.tab, TabId(8), "the tab that followed the closed one");
}

#[test]
/// Closing a whole session lands on the session beside it.
///
/// # Panics
///
/// Panics when the next session is not the one that followed, or the previous
/// one when the closed session was last.
fn closing_a_session_opens_the_session_beside_it() {
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let mut state = settled_three_sessions(&host).expect("three sessions encode");
    let mut place =
        bars::tab_place(&state, &tab_key(SessionId(2), TabId(5))).expect("session 2 is present");
    apply_session_removed(&mut state, &host, SessionId(2), Generation(2))
        .expect("session removal encodes");
    let next = bars::tab_after_close(&state, &place).expect("session 3 follows");
    assert_eq!(
        next.session,
        SessionId(3),
        "the session after the closed one"
    );
    assert_eq!(next.tab, TabId(7), "that session's first tab");

    state = settled_three_sessions(&host).expect("three sessions encode");
    place =
        bars::tab_place(&state, &tab_key(SessionId(3), TabId(9))).expect("session 3 is present");
    apply_session_removed(&mut state, &host, SessionId(3), Generation(2))
        .expect("session removal encodes");
    let previous = bars::tab_after_close(&state, &place).expect("session 2 is before session 3");
    assert_eq!(
        previous.session,
        SessionId(2),
        "the session before the closed one"
    );
    assert_eq!(previous.tab, TabId(4), "that session's first tab");
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
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let bars = bars::render(context.theme(), &self.state, None, None);
        div().child(bars.top).child(bars.bottom)
    }
}

/// With no host held the session strip says so and the tab strip offers nothing.
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

/// A tab past the strip starts hidden, a wheel brings it into view, and
/// scrolling back to the first chip hides it again.
#[gpui_kit::test]
fn tab_strip_scroll_shows_a_chip_past_the_window(context: &mut TestAppContext) {
    let result = scrolled_tabs(context);
    check(&result);
}

/// Every chip drawn in either bar is held within that bar.
///
/// A session chip carries a border and a close affordance the bar must fit,
/// and a bar too short for them cuts its own entries. Chips wider than the
/// window are the bars' own clipped overflow, never a chip drawn past them.
#[gpui_kit::test]
fn bar_entries_fit_within_their_bars(context: &mut TestAppContext) {
    let result = entries_fit(context);
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
    context.update(gpui_kit::init);
    let handle = context.add_window(|_, _| BarsFixture {
        state: EngineState::new(),
    });
    draw(context, handle)?;
    context.update_window(handle.into(), |_, window, _| {
        if window.try_find("tab-new").is_some() {
            return Err("an empty tab bar offers no new tab".into());
        }
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
    context.update(gpui_kit::init);
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

/// A settled model of three sessions, each with three tabs.
///
/// # Errors
///
/// Returns the encoding error when the fixture model cannot be written.
fn settled_three_sessions(
    host: &iznik_client::host::identity::HostId,
) -> Result<EngineState, Box<dyn std::error::Error>> {
    let mut state = EngineState::new();
    let model = three_session_model();
    let payload = encode_host_model(&model)?;
    state.apply(
        host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload,
        },
    );
    Ok(state)
}

/// Identity of one tab in the three-session fixture.
fn tab_key(session: SessionId, tab: TabId) -> TabKey {
    TabKey {
        host: iznik_client::host::identity::HostId("build".to_owned()),
        session,
        tab,
    }
}

/// Apply one tab removal at `generation`.
///
/// # Errors
///
/// Returns the encoding error when the delta cannot be written.
fn apply_tab_removed(
    state: &mut EngineState,
    host: &iznik_client::host::identity::HostId,
    tab: TabId,
    generation: Generation,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = encode_delta(&Delta::TabRemoved { tab })?;
    state.apply(
        host,
        &ToClient::Delta {
            generation,
            payload,
        },
    );
    Ok(())
}

/// Apply one session removal at `generation`.
///
/// # Errors
///
/// Returns the encoding error when the delta cannot be written.
fn apply_session_removed(
    state: &mut EngineState,
    host: &iznik_client::host::identity::HostId,
    session: SessionId,
    generation: Generation,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = encode_delta(&Delta::SessionRemoved { session })?;
    state.apply(
        host,
        &ToClient::Delta {
            generation,
            payload,
        },
    );
    Ok(())
}

/// Three sessions, each with three tabs, numbered in order from 1.
fn three_session_model() -> HostModel {
    let sessions = (1_u64..=3)
        .map(|session| {
            let first = session.saturating_mul(3).saturating_sub(2);
            Session {
                id: SessionId(session),
                name: format!("session {session}"),
                tabs: (first..first.saturating_add(3))
                    .map(|tab| {
                        let pane = PaneId(tab.saturating_add(20));
                        Tab {
                            id: TabId(tab),
                            name: format!("tab {tab}"),
                            panes: vec![Pane {
                                id: pane,
                                title: String::new(),
                                working_directory: None,
                                columns: 80,
                                rows: 24,
                            }],
                            layout: LayoutNode::Leaf(pane),
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    HostModel {
        generation: Generation(1),
        sessions,
    }
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
fn draw<View: Render>(
    context: &mut TestAppContext,
    handle: WindowHandle<View>,
) -> Result<(), Box<dyn std::error::Error>> {
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    Ok(())
}

/// The UI font size the application window uses, in pixels.
///
/// The bars are sized in rems of the window font. A test at the harness
/// default of 16 does not measure the size the running window draws.
const CHROME_FONT_SIZE: f32 = 14.0;

/// Render many sessions and assert each drawn chip is held within its bar.
///
/// The first session and the only drawn tab must sit inside their bar, border
/// included. A session past the bar's right edge must not be drawn at all.
///
/// # Errors
/// Returns a model encoding or closed-window error.
fn entries_fit(context: &mut TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    let mut state = EngineState::new();
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let model = many_session_model();
    let payload = encode_host_model(&model)?;
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload,
        },
    );
    context.update(gpui_kit::init);
    let handle = context.add_window(|_, _| BarsFixture { state });
    context.update_window(handle.into(), |_, window, _| {
        window.set_rem_size(px(CHROME_FONT_SIZE));
    })?;
    draw(context, handle)?;
    context.update_window(handle.into(), |_, window, _| {
        let session_bar = window.find("session-bar").bounds();
        let tab_bar = window.find("tab-bar").bounds();
        for (bar, prefix) in [(session_bar, "session-build-2"), (tab_bar, "tab-build-3")] {
            let bounds = window.find(prefix).bounds();
            if bounds.top() < bar.top() || bounds.bottom() > bar.bottom() {
                return Err(format!(
                    "{prefix} escapes its bar vertically: chip {bounds:?} bar {bar:?}"
                )
                .into());
            }
        }
        let last = window.find("session-build-590002");
        if last.visible() && last.bounds().right() > session_bar.right() {
            return Err("a session chip is drawn past the bar's right edge".into());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })??;
    Ok(())
}

/// A root that renders one session's tabs in a strip that records its scroll offset.
struct TabScrollFixture {
    /// The model state shown by the bars.
    state: EngineState,
    /// The strip's horizontal offset.
    scroll: ScrollHandle,
}

impl Render for TabScrollFixture {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let bars = bars::render_placed(
            context.theme(),
            &self.state,
            None,
            None,
            bars::TabPlacement::Bar,
            ShortcutHint::None,
            Some(&self.scroll),
        );
        div().size_full().child(bars.top).child(bars.bottom)
    }
}

/// Draw more tabs than fit and scroll the strip to either end.
///
/// # Errors
/// Returns a model encoding or closed-window error.
fn scrolled_tabs(context: &mut TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    /// Narrow enough that the long tab names cannot all sit in the strip.
    const WINDOW_WIDTH: f32 = 480.0;
    /// Tall enough for the tab strip and the session strip.
    const WINDOW_HEIGHT: f32 = 200.0;
    /// A vertical wheel over the strip; the strip turns it into a horizontal move.
    const SCROLL_DISTANCE: f32 = -8_000.0;
    let mut state = EngineState::new();
    let host = iznik_client::host::identity::HostId("build".to_owned());
    let model = many_tab_model();
    let first = model
        .sessions
        .first()
        .and_then(|session| session.tabs.first())
        .map(|tab| tab.id.0)
        .ok_or("the fixture has no first tab")?;
    let last = model
        .sessions
        .first()
        .and_then(|session| session.tabs.last())
        .map(|tab| tab.id.0)
        .ok_or("the fixture has no last tab")?;
    let payload = encode_host_model(&model)?;
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload,
        },
    );
    let scroll = ScrollHandle::new();
    context.update(gpui_kit::init);
    let handle = context.open_window(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), {
        let scroll = scroll.clone();
        move |_, _| TabScrollFixture { state, scroll }
    });
    context.update_window(handle.into(), |_, window, _| {
        window.set_rem_size(px(CHROME_FONT_SIZE));
    })?;
    draw(context, handle)?;
    let first_id = format!("tab-build-{first}");
    let last_id = format!("tab-build-{last}");
    context.update_window(handle.into(), |_, window, application| {
        if !window.find(first_id.clone()).visible() {
            return Err("the first tab starts off the strip".into());
        }
        if window.find(last_id.clone()).visible() {
            return Err("a tab past the strip is drawn before scrolling".into());
        }
        window.scroll(
            "tab-bar",
            ScrollDelta::Pixels(point(px(0.), px(SCROLL_DISTANCE))),
            application,
        );
        if !window.find(last_id.clone()).visible() {
            return Err("scrolling did not bring the last tab into the strip".into());
        }
        if window.find(first_id.clone()).visible() {
            return Err("scrolling left the first tab on the strip".into());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })??;
    scroll.scroll_to_item(0);
    draw(context, handle)?;
    context.update_window(handle.into(), |_, window, _| {
        if !window.find(first_id.clone()).visible() {
            return Err("the selected tab did not scroll back into the strip".into());
        }
        if window.find(last_id.clone()).visible() {
            return Err("scrolling to the first tab left the last tab on the strip".into());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })??;
    Ok(())
}

/// One session with more tabs than fit a narrow window.
fn many_tab_model() -> HostModel {
    const TAB_COUNT: u64 = 24;
    const FIRST_TAB: u64 = 3;
    let tabs = (0..TAB_COUNT)
        .map(|index| {
            let id = FIRST_TAB.saturating_add(index);
            let pane = PaneId(id.saturating_add(100));
            Tab {
                id: TabId(id),
                name: format!("documentation tab {index}"),
                panes: vec![Pane {
                    id: pane,
                    title: "shell".to_owned(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(pane),
            }
        })
        .collect();
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(2),
            name: "work".to_owned(),
            tabs,
        }],
    }
}

/// A host with more sessions than fit the window, each holding one tab.
fn many_session_model() -> HostModel {
    const SESSION_COUNT: u64 = 60;
    const IDENTITIES_PER_SESSION: u64 = 10_000;
    let sessions = (0..SESSION_COUNT)
        .map(|index| {
            let base = index.saturating_mul(IDENTITIES_PER_SESSION);
            let pane = PaneId(base.saturating_add(4));
            Session {
                id: SessionId(base.saturating_add(2)),
                name: format!("session {index}"),
                tabs: vec![Tab {
                    id: TabId(base.saturating_add(3)),
                    name: "shell".to_owned(),
                    panes: vec![Pane {
                        id: pane,
                        title: "shell".to_owned(),
                        working_directory: None,
                        columns: 80,
                        rows: 24,
                    }],
                    layout: LayoutNode::Leaf(pane),
                }],
            }
        })
        .collect();
    HostModel {
        generation: Generation(1),
        sessions,
    }
}
