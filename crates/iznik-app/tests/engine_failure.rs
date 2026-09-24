//! A failure the engine reports, or a refusal of a command a person issued,
//! reaches the window as a banner, instead of being kept where nothing reads it.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, Context, TestAppContext, Window};
use iznik_app::actions::ActionId;
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::prompt;
use iznik_app::surface::PasteConfirmation;
use iznik_app::vt::{PaneKey, VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::command::{CommandOutcome, RejectionCode};
use iznik_protocol::identity::{CommandId, PaneId};

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// A command the host never answered is shown, not only recorded.
#[gpui_kit::test]
fn an_unanswered_command_is_shown_as_a_failure(context: &mut TestAppContext) {
    check(&shown(
        context,
        "engine-failure",
        Notification::CommandTimedOut {
            host: HostId("devbox".to_owned()),
            command: CommandId(1),
        },
    ));
}

/// A command the host refused is shown to the person who issued it.
#[gpui_kit::test]
fn a_refused_command_is_shown(context: &mut TestAppContext) {
    check(&shown(
        context,
        "engine-rejection",
        Notification::CommandFinished {
            host: HostId("devbox".to_owned()),
            command: CommandId(1),
            outcome: CommandOutcome::Rejected {
                code: RejectionCode::UnknownSession,
                message: "no such session".to_owned(),
            },
        },
    ));
}

/// A multi-line paste that arrives while another question is open is not
/// asked about over it: the open question stays, and the person is told.
#[gpui_kit::test]
fn a_paste_does_not_replace_an_open_question(context: &mut TestAppContext) {
    check(&banner_after(context, "engine-paste", ask_then_paste));
}

/// Open a question, then have a multi-line paste arrive.
///
/// # Panics
/// Fails when the paste replaces the open question.
fn ask_then_paste(
    shell: &mut WindowShell,
    _window: &mut Window,
    application: &mut Context<'_, WindowShell>,
) {
    shell.palette_mut().ask(prompt::add_host_prompt(Vec::new()));
    shell.confirm_paste(
        &PasteConfirmation {
            key: PaneKey {
                host: HostId("devbox".to_owned()),
                pane: PaneId(1),
            },
            text: "one\ntwo".to_owned(),
        },
        application,
    );
    assert_eq!(
        shell
            .palette()
            .prompt
            .as_ref()
            .and_then(|prompt| prompt.action),
        Some(ActionId::AddHost),
        "the open question is kept"
    );
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Open a shell, hand it an engine notice, drain it, and look for the
/// failure banner.
///
/// # Errors
/// Returns setup failures and a closed-window error.
///
/// # Panics
///
/// Panics when the banner is missing.
fn shown(
    context: &mut TestAppContext,
    scratch: &str,
    notification: Notification,
) -> Result<(), Failed> {
    banner_after(context, scratch, |shell, window, application| {
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Notify(notification)),
            window,
            application,
        );
    })
}

/// Open a shell, do `act` to it, drain it, and look for the failure banner.
///
/// # Errors
/// Returns setup failures and a closed-window error.
///
/// # Panics
///
/// Panics when the banner is missing.
fn banner_after(
    context: &mut TestAppContext,
    scratch: &str,
    act: impl FnOnce(&mut WindowShell, &mut Window, &mut Context<'_, WindowShell>),
) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = iznik_testkit::scratch::path(scratch);
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
        act(shell, window, application);
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
