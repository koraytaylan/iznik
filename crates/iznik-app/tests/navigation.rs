//! Command shortcuts that move between tabs and sessions, and the numbers
//! the chips show while Command is held.

#[path = "support/engine.rs"]
mod engine;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Modifiers, TestAppContext, WindowHandle};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

/// Fixture setup and window failures.
type Failed = Box<dyn std::error::Error>;

/// The host the fixture snapshot is applied to.
fn host() -> HostId {
    HostId("build".to_owned())
}

#[gpui_kit::test]
fn command_shortcut_moves_between_tabs_and_sessions(context: &mut TestAppContext) {
    check(&moves(context));
}

/// Assert a shortcut case without making the GPUI test macro own its error path.
///
/// # Panics
/// Fails with the underlying window error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Open three sessions and walk the Command shortcuts and the numbered chips.
///
/// # Errors
/// Propagates fixture, encoding and window failures.
///
/// # Panics
/// Fails when a shortcut lands on the wrong tab, or a chip shows the wrong number.
fn moves(context: &mut TestAppContext) -> Result<(), Failed> {
    let (handle, _directory) = open(context)?;
    draw(context, handle)?;
    step_tabs(context, handle)?;
    hold(context, handle, Modifiers::command())?;
    assert_eq!(
        chip_label(context, handle, "tab-build-1")?,
        "1 tab 1 \u{b7} unavailable",
        "command leads the first tab with its number"
    );
    assert_eq!(
        chip_label(context, handle, "tab-build-3")?,
        "3 tab 3 \u{b7} unavailable",
        "command leads the third tab with its number"
    );
    assert_eq!(
        chip_label(context, handle, "session-build-1")?,
        "session 1",
        "command leaves the session chips unnumbered"
    );
    hold(context, handle, Modifiers::command() | Modifiers::alt())?;
    assert_eq!(
        chip_label(context, handle, "session-build-2")?,
        "2 session 2",
        "command-option leads the second session with its number"
    );
    assert_eq!(
        chip_label(context, handle, "tab-build-2")?,
        "tab 2 \u{b7} unavailable",
        "command-option leaves the tab chips unnumbered"
    );
    press(context, handle, "cmd-alt-2");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(2), TabId(4)),
        "command-option-2 opens the second session"
    );
    press(context, handle, "cmd-alt-up");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(7)),
        "command-option-up opens the next session"
    );
    press(context, handle, "cmd-alt-right");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(2)),
        "command-option-right wraps to the session left on its tab"
    );
    press(context, handle, "cmd-alt-down");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(7)),
        "command-option-down wraps to the previous session"
    );
    press(context, handle, "2");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(7)),
        "a digit without command stays with the terminal"
    );
    hold(context, handle, Modifiers::none())?;
    assert_eq!(
        chip_label(context, handle, "session-build-3")?,
        "session 3",
        "releasing command clears the session numbers"
    );
    press(context, handle, "ctrl-tab");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(3), TabId(8)),
        "control-tab still opens the next tab"
    );
    Ok(())
}

/// Command digits and arrows move among the three tabs, wrapping at either end.
///
/// # Errors
/// Propagates window failures.
///
/// # Panics
/// Fails when a shortcut lands on the wrong tab.
fn step_tabs(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
) -> Result<(), Failed> {
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(1)),
        "the window opens the first tab"
    );
    press(context, handle, "cmd-9");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(1)),
        "a digit past the last tab leaves the selection"
    );
    press(context, handle, "cmd-2");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(2)),
        "command-2 opens the second tab"
    );
    press(context, handle, "cmd-right");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(3)),
        "command-right opens the next tab"
    );
    press(context, handle, "cmd-up");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(1)),
        "command-up wraps to the first tab"
    );
    press(context, handle, "cmd-left");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(3)),
        "command-left wraps to the last tab"
    );
    press(context, handle, "cmd-down");
    assert_eq!(
        shown(context, handle)?,
        (SessionId(1), TabId(2)),
        "command-down opens the previous tab"
    );
    Ok(())
}

/// Draw the shell so its key listeners are installed.
///
/// # Errors
/// Returns a closed-window failure.
fn draw(context: &mut TestAppContext, handle: WindowHandle<WindowShell>) -> Result<(), Failed> {
    context.update_window(handle.into(), |_, window, application| {
        window.render_frame(application);
    })?;
    Ok(())
}

/// Send one chord to the shell.
fn press(context: &mut TestAppContext, handle: WindowHandle<WindowShell>, chord: &str) {
    context.simulate_keystrokes(handle.into(), chord);
}

/// Hold or release Command and draw the chips again.
///
/// # Errors
/// Returns a closed-window failure.
fn hold(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    modifiers: Modifiers,
) -> Result<(), Failed> {
    context.update_window(handle.into(), |_, window, application| {
        window.render_frame(application);
        window.dispatch_event(
            gpui_kit::PlatformInput::ModifiersChanged(gpui_kit::ModifiersChangedEvent {
                modifiers,
                ..gpui_kit::ModifiersChangedEvent::default()
            }),
            application,
        );
        window.render_frame(application);
    })?;
    Ok(())
}

/// The accessibility name of one chip.
///
/// # Errors
/// Returns a closed-window failure, or an error when the chip has no name.
fn chip_label(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    identifier: &str,
) -> Result<String, Failed> {
    context.update_window(handle.into(), |_, window, _application| {
        window
            .find(identifier.to_owned())
            .label()
            .map(str::to_owned)
            .ok_or_else(|| format!("chip {identifier} has no label").into())
    })?
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

/// Open a shell whose model is three sessions of three tabs.
///
/// # Errors
/// Propagates fixture, encoding and window failures.
fn open(
    context: &mut TestAppContext,
) -> Result<(WindowHandle<WindowShell>, engine::Directory), Failed> {
    context.update(gpui_kit::init);
    let (bridge, directory) = engine::start("navigation")?;
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
    handle.update(context, |shell, window, context| {
        shell.absorb(
            iznik_app::bridge::EngineEvent::Said(ManagerEvent::Snapshot {
                host: host(),
                generation: Generation(1),
                payload: encode_host_model(&three_sessions())?,
            }),
            window,
            context,
        );
        Ok::<(), Failed>(())
    })??;
    Ok((handle, directory))
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
