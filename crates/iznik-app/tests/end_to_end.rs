//! Headless application assembly proof over the model and rendered chrome.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, TestAppContext,
    TestSupportExt, Window, WindowHandle,
};
use iznik_app::actions::ActionId;
use iznik_app::bars;
use iznik_app::bridge::EngineBridge;
use iznik_app::host_ui::EngineState;
use iznik_app::palette::Palette;
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};
use iznik_testkit::stack::{Stack, StackOptions};

/// Polling interval for the live stack fixture.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Maximum time allowed for a local host and command delta to arrive.
const LIVE_DEADLINE: Duration = Duration::from_secs(10);

/// A GPUI root that composes the application chrome from one model state.
struct ApplicationFixture {
    /// The authoritative model mirror shown by the fixture.
    state: EngineState,
    /// The palette overlay state shown above the bars.
    palette: Palette,
}

impl Render for ApplicationFixture {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        gpui_kit::div()
            .id("application-root")
            .test_support()
            .child(bars::render(&self.state, None, None))
            .child(iznik_app::palette::render(&self.state, &self.palette, None))
    }
}

/// The headless application renders a session, opens its palette, and follows a removal delta.
#[gpui_kit::test]
fn application_assembly_follows_model_deltas(context: &mut TestAppContext) {
    check(&assembly(context));
}

/// The production shell accepts a local host and palette session command in a headless window.
#[gpui_kit::test]
fn application_shell_drives_the_in_process_stack(context: &mut TestAppContext) {
    check(&live_stack(context));
}

/// Start the real local daemon, attach the production shell, and await a palette command delta.
///
/// # Errors
/// Returns setup, engine, window, or deadline errors.
///
/// # Panics
/// Panics when the live session bar is absent after the command delta.
fn live_stack(context: &mut TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    context.update(gpui_kit::init);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
    let directory = temporary_directory()?;
    let bridge = EngineBridge::start(
        directory.join("artifacts"),
        ClientRuntimePaths::under(&directory.join("runtime"))?,
    )?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let alias = HostId(format!("unix:{}", stack.socket().display()));
    let handle = context.add_window(|window, application| {
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
    handle.update(context, |shell, _, _| shell.hosts_mut().add_host(&alias.0))??;
    wait_for(context, handle, |shell| {
        shell.hosts().state().host(&alias).is_some()
    })?;
    handle.update(context, |shell, _, _| {
        shell.dispatch_action(ActionId::CreateSession)
    })??;
    wait_for(context, handle, |shell| {
        shell
            .hosts()
            .state()
            .model()
            .host(&alias)
            .is_some_and(|host| !host.model.sessions.is_empty())
    })?;
    handle.update(context, |shell, window, application| {
        let host = shell
            .hosts()
            .state()
            .model()
            .host(&alias)
            .ok_or("host missing")?;
        let session = host.model.sessions.first().ok_or("session missing")?;
        let tab = session.tabs.first().ok_or("tab missing")?;
        let key = iznik_app::window::TabKey {
            host: alias.clone(),
            session: session.id,
            tab: tab.id,
        };
        if shell.select(key, window, application) {
            Ok::<(), Box<dyn std::error::Error>>(())
        } else {
            Err("tab could not be selected".into())
        }
    })??;
    handle.update(context, |shell, _, _| {
        shell.dispatch_action(ActionId::CreatePane)
    })??;
    wait_for(context, handle, |shell| {
        shell
            .hosts()
            .state()
            .model()
            .host(&alias)
            .and_then(|host| host.model.sessions.first())
            .and_then(|session| session.tabs.first())
            .is_some_and(|tab| tab.panes.len() > 1)
    })?;
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
        assert!(
            window.try_find("session-bar").is_some(),
            "the live session is rendered in the session bar"
        );
    })?;
    handle.update(context, |shell, _, _| {
        shell.dispatch_action(ActionId::RemoveHost)
    })??;
    wait_for(context, handle, |shell| {
        shell.hosts().state().host(&alias).is_none()
    })?;
    let _removed = std::fs::remove_dir_all(directory);
    drop(stack);
    Ok(())
}

/// Drive shell updates until a live-stack predicate becomes true.
///
/// # Errors
/// Returns a closed-window error or the live deadline error.
fn wait_for(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    predicate: impl Fn(&WindowShell) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(deadline) = Instant::now().checked_add(LIVE_DEADLINE) else {
        return Err("live stack deadline could not be represented".into());
    };
    loop {
        let reached = handle.update(context, |shell, window, application| {
            shell.update(window, application);
            predicate(shell)
        })?;
        if reached {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("live stack deadline expired".into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Allocate isolated engine paths for the live application fixture.
///
/// # Errors
/// Returns filesystem errors.
fn temporary_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("iznik-app-e2e-{}", std::process::id()));
    std::fs::remove_dir_all(&directory).ok();
    std::fs::create_dir_all(directory.join("artifacts"))?;
    Ok(directory)
}

/// Drive one snapshot, palette projection, and authoritative tab removal.
///
/// # Errors
/// Returns encoding or closed-window errors.
fn assembly(context: &mut TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    let host = HostId("build".to_owned());
    let model = model();
    let mut state = EngineState::new();
    state.apply(
        &host,
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    );
    let handle = context.add_window(|_, _| ApplicationFixture {
        state,
        palette: Palette {
            open: true,
            ..Palette::default()
        },
    });
    draw(context, handle)?;
    context.update_window(handle.into(), |_, window, _| {
        window.find("session-build-2").visible();
        window.find("tab-build-3").visible();
        window.find("command-palette").visible();
    })?;
    let removal = encode_delta(&Delta::TabRemoved { tab: TabId(3) })?;
    handle.update(context, |fixture, _window, _application| {
        fixture.state.apply(
            &host,
            &ToClient::Delta {
                generation: Generation(2),
                payload: removal,
            },
        );
    })?;
    draw(context, handle)?;
    let check = context.update_window(handle.into(), |_, window, _| {
        window
            .try_find("session-build-2")
            .ok_or("session disappeared unexpectedly")?;
        if window.try_find("tab-build-3").is_some() {
            return Err("removed tab remains visible".into());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    check?;
    Ok(())
}

/// Assert that the application assembly completed without an error.
///
/// # Panics
/// Panics when the assembled application does not match the expected model projection.
fn check(result: &Result<(), Box<dyn std::error::Error>>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Build the smallest complete host model the application can render.
fn model() -> HostModel {
    HostModel {
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
    }
}

/// Draw a fixture and flush its element tree.
///
/// # Errors
/// Returns the closed-window error.
fn draw(
    context: &mut TestAppContext,
    handle: WindowHandle<ApplicationFixture>,
) -> Result<(), Box<dyn std::error::Error>> {
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    Ok(())
}
