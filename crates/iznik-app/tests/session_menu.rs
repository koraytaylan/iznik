//! A session's right click opens its menu, and the menu stays open across the
//! window's own timed repaints.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, TestAppContext, WindowHandle};
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::HostState;
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// The frames a real window draws before the person sees the menu: at the
/// shell's sixteen-millisecond cadence this is a fraction of a second.
const REPAINTS: usize = 3;

/// A right click on a session opens its menu, and the menu is still there
/// after the window's own timed repaints.
#[gpui_kit::test]
fn a_session_right_click_opens_a_menu_that_survives_repaints(context: &mut TestAppContext) {
    check(&opened(context));
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// A shell over one settled session, right-clicked, then redrawn.
///
/// # Errors
/// Returns setup, encoding, or closed-window failures.
///
/// # Panics
///
/// Panics when the session chip is not drawn.
fn opened(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory: PathBuf =
        std::env::temp_dir().join(format!("iznik-session-menu-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("artifacts"))?;
    let bridge = EngineBridge::start(
        directory.join("artifacts"),
        ClientRuntimePaths::under(&directory.join("runtime"))?,
    )?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle: WindowHandle<WindowShell> = context.add_window(|window, application| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ..ShellOptions::default()
            },
            window,
            application,
        )
    });
    settle(context, handle)?;
    context.update_window(handle.into(), |_, window, application| {
        assert!(
            window.try_find("session-build-2").is_some(),
            "the session is drawn"
        );
        window.right_click("session-build-2", application);
    })?;
    context.run_until_parked();
    for _frame in 0..REPAINTS {
        context.update_window(handle.into(), |_, window, application| {
            window.draw(application).clear(application);
            assert!(
                window.try_find("session-menu").is_some(),
                "the menu is still open after a repaint"
            );
        })?;
        context.run_until_parked();
    }
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Give the shell a connected host whose model holds one session.
///
/// # Errors
/// Returns encoding or window failures.
fn settle(context: &mut TestAppContext, handle: WindowHandle<WindowShell>) -> Result<(), Failed> {
    let alias = HostId("build".to_owned());
    handle.update(context, |shell, window, application| {
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Moved {
                host: alias.clone(),
                state: HostState::Connected {
                    server_version: "0.0.0".to_owned(),
                    capabilities: Capabilities::REORDER_SESSIONS,
                    upgrade: None,
                },
            }),
            window,
            application,
        );
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
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Snapshot {
                host: alias,
                generation: model.generation,
                payload,
            }),
            window,
            application,
        );
        Ok::<(), Failed>(())
    })??;
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    context.run_until_parked();
    Ok(())
}
