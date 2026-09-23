//! A failure the engine reports reaches the window as a banner, instead of
//! being kept where nothing reads it.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, TestAppContext};
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::identity::CommandId;

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// A command the host never answered is shown, not only recorded.
#[gpui_kit::test]
fn an_unanswered_command_is_shown_as_a_failure(context: &mut TestAppContext) {
    check(&shown(context));
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Open a shell, hand it an engine notice that a command timed out, drain it,
/// and look for the failure banner.
///
/// # Errors
/// Returns setup failures and a closed-window error.
///
/// # Panics
///
/// Panics when the banner is missing.
fn shown(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = iznik_testkit::scratch::path("engine-failure");
    std::fs::create_dir_all(directory.join("artifacts"))?;
    let (bridge, thread) = owners(&directory)?;
    let created: std::cell::RefCell<Option<gpui_kit::Entity<WindowShell>>> =
        std::cell::RefCell::new(None);
    let (_root, context) = context.add_window_view(|window, context| {
        let shell = context.new(|context| {
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
        *created.borrow_mut() = Some(shell.clone());
        gpui_kit::component::Root::new(shell, window, context)
    });
    let shell = created.borrow().clone().ok_or("the shell was not built")?;
    shell.update_in(context, |shell, window, application| {
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Notify(Notification::CommandTimedOut {
                host: HostId("devbox".to_owned()),
                command: CommandId(1),
            })),
            window,
            application,
        );
        shell.update(window, application);
    });
    context.update(|window, _application| {
        window.find("surface-failure").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Assemble the bridge and terminal owner a headless shell needs, without
/// connecting a host.
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
