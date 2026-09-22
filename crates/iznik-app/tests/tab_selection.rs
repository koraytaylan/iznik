//! Closing a tab keeps the window on that session, and switching sessions
//! returns to the tab each session was left on.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, WindowHandle};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, TabKey, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

#[path = "support/engine.rs"]
mod engine;

/// Fixture setup and window failures.
type Failed = Box<dyn std::error::Error>;

/// The host the fixture snapshot is applied to.
fn host() -> HostId {
    HostId("build".to_owned())
}

#[gpui_kit::test]
fn closing_a_tab_stays_in_its_session(context: &mut TestAppContext) {
    check(&stays(context));
}

#[gpui_kit::test]
fn returning_to_a_session_shows_the_tab_it_was_left_on(context: &mut TestAppContext) {
    check(&returns_to_the_tab(context));
}

/// Leave a session on a later tab and come back to it by clicking its chip.
///
/// # Errors
/// Propagates fixture, encoding and window failures.
///
/// # Panics
/// Fails when a return opens the first tab, or when a session that was never
/// opened does so for any tab but its first.
fn returns_to_the_tab(context: &mut TestAppContext) -> Result<(), Failed> {
    let (handle, _directory) = open(context)?;
    choose(context, handle, 2, 5)?;
    click_session(context, handle, 1)?;
    assert_eq!(shown(context, handle)?, (SessionId(1), TabId(1)));
    click_session(context, handle, 2)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(5)),
        "session 2 opens the tab it was left on"
    );
    click_session(context, handle, 3)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(7)),
        "a session opened for the first time shows its first tab"
    );
    choose(context, handle, 3, 8)?;
    click_session(context, handle, 1)?;
    click_session(context, handle, 3)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(8)),
        "session 3 opens the tab it was left on"
    );
    click_session(context, handle, 1)?;
    remove(context, handle, 5, 2)?;
    assert_eq!(shown(context, handle)?, (SessionId(1), TabId(1)));
    click_session(context, handle, 2)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(4)),
        "a tab closed while away yields the session's first tab"
    );
    Ok(())
}

/// Click one session chip of the fixture.
///
/// # Errors
/// Returns a closed-window failure.
fn click_session(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    session: u64,
) -> Result<(), Failed> {
    let chip = format!("session-build-{session}");
    context.update_window(handle.into(), |_, window, application| {
        window.click(chip, application);
    })?;
    Ok(())
}

/// Assert a selection case without making the GPUI test macro own its error path.
///
/// # Panics
/// Fails with the underlying window error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Close tabs in the second of three sessions and read where the window lands.
///
/// # Errors
/// Propagates fixture, encoding and window failures.
///
/// # Panics
/// Fails when a close leaves the first session, or when the close mark selects
/// the tab it closes.
fn stays(context: &mut TestAppContext) -> Result<(), Failed> {
    let (handle, _directory) = open(context)?;
    choose(context, handle, 2, 5)?;
    context.update_window(handle.into(), |_, window, application| {
        window.click("tab-close-build-4", application);
    })?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(5)),
        "closing another tab does not select it"
    );
    remove(context, handle, 5, 2)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(6)),
        "the tab that followed the closed one stays in session 2"
    );
    remove(context, handle, 8, 3)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(6)),
        "closing a tab in another session leaves this one on screen"
    );
    remove(context, handle, 6, 4)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(4)),
        "the tab before the closed one, still in session 2"
    );
    remove(context, handle, 4, 5)?;
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(7)),
        "the last tab of session 2 opens the next session"
    );
    Ok(())
}

/// Open a shell whose model is three sessions of three tabs.
///
/// # Errors
/// Propagates fixture, encoding and window failures.
fn open(
    context: &mut TestAppContext,
) -> Result<(WindowHandle<WindowShell>, engine::Directory), Failed> {
    context.update(gpui_kit::init);
    let (bridge, directory) = engine::start("tab-selection")?;
    let thread = std::rc::Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, context| {
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
    let model = three_sessions();
    absorb(
        context,
        handle,
        ManagerEvent::Snapshot {
            host: host(),
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    )?;
    Ok((handle, directory))
}

/// Select one tab of the fixture.
///
/// # Errors
/// Returns a closed-window failure, or an error when the tab is not there.
fn choose(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    session: u64,
    tab: u64,
) -> Result<(), Failed> {
    let selected = handle.update(context, |shell, window, context| {
        shell.select(key(session, tab), window, context)
    })?;
    if selected {
        Ok(())
    } else {
        Err("tab could not be selected".into())
    }
}

/// The session and tab on screen.
///
/// # Errors
/// Returns a closed-window failure, or an error when nothing is selected.
fn shown(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
) -> Result<(SessionId, TabId), Failed> {
    handle
        .update(context, |shell, _, _| {
            shell
                .selected()
                .map(|selected| (selected.session, selected.tab))
        })?
        .ok_or_else(|| "nothing is selected".into())
}

/// Apply one authoritative tab removal.
///
/// # Errors
/// Propagates encoding and window failures.
fn remove(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    tab: u64,
    generation: u64,
) -> Result<(), Failed> {
    absorb(
        context,
        handle,
        ManagerEvent::Delta {
            host: host(),
            generation: Generation(generation),
            payload: encode_delta(&Delta::TabRemoved { tab: TabId(tab) })?,
        },
    )
}

/// Submit one model event through the shell.
///
/// # Errors
/// Returns a closed-window failure.
fn absorb(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    event: ManagerEvent,
) -> Result<(), Failed> {
    handle.update(context, |shell, window, context| {
        shell.absorb(iznik_app::bridge::EngineEvent::Said(event), window, context);
    })?;
    Ok(())
}

/// A host-qualified tab in the fixture.
fn key(session: u64, tab: u64) -> TabKey {
    TabKey {
        host: host(),
        session: SessionId(session),
        tab: TabId(tab),
    }
}

/// Three sessions, each with three tabs, numbered in order from 1.
fn three_sessions() -> HostModel {
    let sessions = (1_u64..=3)
        .map(|session| {
            let first = session.saturating_mul(3).saturating_sub(2);
            Session {
                id: SessionId(session),
                name: format!("session {session}"),
                tabs: (first..first.saturating_add(3)).map(one_tab).collect(),
            }
        })
        .collect();
    HostModel {
        generation: Generation(1),
        sessions,
    }
}

/// One leaf tab whose pane number does not collide with its tab number.
fn one_tab(tab: u64) -> Tab {
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
}
