//! The developer tools window lists the model the shell holds.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, MenuItem, TestAppContext, VisualContext, VisualTestContext};
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::host_ui::EngineState;
use iznik_app::menu;
use iznik_app::tools_window::{self, report_lines};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::HostState;
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;

/// The host the fixture connects.
const HOST: &str = "fixture";
/// The session the fixture holds.
const SESSION_NAME: &str = "work";
/// The pane the fixture holds.
const PANE_NUMBER: u64 = 7;

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// The view menu offers Developer Tools.
///
/// # Panics
/// Panics when the item is missing.
#[test]
fn view_menu_offers_the_tools() {
    let menus = menu::menu_bar();
    let view = menus
        .iter()
        .find(|entry| entry.name.as_ref() == "View")
        .expect("view menu");
    let offered = view.items.iter().any(
        |item| matches!(item, MenuItem::Action { name, .. } if name.as_ref() == "Developer Tools"),
    );
    assert!(offered, "View is missing Developer Tools");
}

/// A connected host is described by its server, its capabilities and its pane.
///
/// # Panics
/// Panics when a fact of the model is missing.
#[test]
fn report_lines_names_the_connection_and_the_pane() {
    let state = connected_state().expect("model encodes");
    let lines = report_lines(&state, None, None);
    let text = lines.join("\n");
    assert!(text.contains(HOST), "{text}");
    assert!(text.contains("connected to 0.0.0"), "{text}");
    assert!(text.contains("reorder"), "{text}");
    assert!(text.contains("adopt"), "{text}");
    assert!(
        text.contains(&format!("session 1 {SESSION_NAME}")),
        "{text}"
    );
    assert!(text.contains(&format!("pane {PANE_NUMBER} ")), "{text}");
    assert!(text.contains("/tmp/work"), "{text}");
}

/// An empty engine says that no host is held.
///
/// # Panics
/// Panics when it says something else.
#[test]
fn report_lines_for_no_host() {
    let lines = report_lines(&EngineState::new(), None, None);
    assert_eq!(
        lines,
        vec!["No host is held.".to_owned()],
        "an empty engine says no host is held"
    );
}

/// Opening the window draws the report of the model the shell holds.
#[gpui_kit::test]
fn tools_window_shows_the_held_model(context: &mut TestAppContext) {
    check(&shows_the_model(context));
}

/// A state with one connected host and one pane.
///
/// # Errors
/// Returns the model encoding error.
fn connected_state() -> Result<EngineState, Failed> {
    let mut state = EngineState::new();
    let host = HostId(HOST.to_owned());
    state.absorb(EngineEvent::Said(ManagerEvent::Moved {
        host: host.clone(),
        state: HostState::Connected {
            server_version: "0.0.0".to_owned(),
            capabilities: Capabilities::known(),
            upgrade: None,
        },
    }));
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: SESSION_NAME.to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                panes: vec![Pane {
                    id: PaneId(PANE_NUMBER),
                    title: "shell".to_owned(),
                    working_directory: Some("/tmp/work".to_owned()),
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(PaneId(PANE_NUMBER)),
            }],
        }],
    };
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    );
    Ok(state)
}

/// Assemble the bridge and terminal owner a headless shell needs.
///
/// # Errors
/// Returns the bridge or terminal owner's startup errors.
fn owners(directory: &Path) -> Result<(EngineBridge, Rc<VtThread>), Failed> {
    let bridge = EngineBridge::start(
        directory.join("artifacts"),
        ClientRuntimePaths::under(&directory.join("runtime"))?,
    )?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    Ok((bridge, thread))
}

/// Allocate an isolated directory for one fixture's engine paths.
///
/// # Errors
/// Returns filesystem errors.
fn temporary_directory(label: &str) -> Result<PathBuf, Failed> {
    let directory = std::env::temp_dir().join(format!(
        "iznik-app-tools-window-{label}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&directory).ok();
    std::fs::create_dir_all(directory.join("artifacts"))?;
    Ok(directory)
}

/// Return the window opened besides `main`.
///
/// # Errors
/// Returns a failure when opening did not add exactly one window.
///
/// # Panics
/// Panics when the window count is not two.
fn opened_window(
    context: &TestAppContext,
    main: AnyWindowHandle,
) -> Result<AnyWindowHandle, Failed> {
    let windows = context.windows();
    assert_eq!(windows.len(), 2, "opening must add exactly one window");
    windows
        .into_iter()
        .find(|window| *window != main)
        .ok_or_else(|| "no window besides the main one".into())
}

/// Put a model on the shell, open the tools window, and read what it drew.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when the window does not show the pane.
fn shows_the_model(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory("open")?;
    let (bridge, thread) = owners(&directory)?;
    let model = connected_state()?;
    let (shell_view, main) = context.add_window_view(|window, context| {
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
    let main_handle = main.window_handle();
    let host_model = model
        .model()
        .host(&HostId(HOST.to_owned()))
        .ok_or("the fixture host is missing")?;
    let payload = encode_host_model(&host_model.model)?;
    shell_view.update_in(main, |shell, window, update_context| {
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Moved {
                host: HostId(HOST.to_owned()),
                state: HostState::Connected {
                    server_version: "0.0.0".to_owned(),
                    capabilities: Capabilities::known(),
                    upgrade: None,
                },
            }),
            window,
            update_context,
        );
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Snapshot {
                host: HostId(HOST.to_owned()),
                generation: Generation(1),
                payload,
            }),
            window,
            update_context,
        );
        tools_window::open(update_context);
    });
    let tools = opened_window(main, main_handle)?;
    let mut tools = VisualTestContext::from_window(tools, main);
    tools.update(|window, application| {
        window.render_frame(application);
        let shown = window.find("developer-hosts");
        let label = shown.label().unwrap_or("");
        assert!(
            label.contains(&format!("pane {PANE_NUMBER} ")),
            "the window lists the pane the host holds: {label}"
        );
        assert!(
            label.contains("connected to 0.0.0"),
            "the window names the connection: {label}"
        );
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}
