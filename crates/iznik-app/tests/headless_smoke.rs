//! The headless smoke: the production window drawn and asserted without a
//! display.
//!
//! GPUI's test context builds a real window that this process renders itself,
//! so this test is what proves the crate's suite runs headlessly under
//! nextest on the Linux gate machine. The window is the one `main` opens: a
//! `WindowShell` over a started engine bridge and emulator thread, with the
//! event-driven pump running, inside the kit's `Root`.

use std::rc::Rc;

use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};

#[path = "support/engine.rs"]
mod engine;

/// Fixture setup failures.
type Failed = Box<dyn std::error::Error>;

#[gpui_kit::test]
fn headless_window_draws_its_element_tree(context: &mut TestAppContext) {
    check(&smoke(context));
}

/// Open the window and assert its drawn tree.
///
/// # Errors
/// Returns why the window would not open.
///
/// # Panics
/// Fails when the drawn tree is not the shell's.
fn smoke(context: &mut TestAppContext) -> Result<(), Failed> {
    let (handle, _directory) = open(context)?;
    assert_the_drawn_surface(context, handle);
    Ok(())
}

/// Keep fixture assertion outside the GPUI macro's generated test documentation.
///
/// # Panics
/// Fails on a fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Open the window as `main` does, with the bundled themes applied.
///
/// # Errors
/// Returns why the engine, the emulator thread or the themes would not start.
fn open(context: &mut TestAppContext) -> Result<(AnyWindowHandle, engine::Directory), Failed> {
    context.update(|app| {
        gpui_kit::init(app);
        iznik_app::menu::install(app);
        iznik_app::theme::apply_default_theme(app)
    })?;
    let (bridge, directory) = engine::start("headless-smoke")?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, build_context| {
        let shell = build_context.new(|shell_context| {
            WindowShell::new(
                bridge,
                thread,
                ShellOptions::default(),
                window,
                shell_context,
            )
        });
        Root::new(shell, window, build_context)
    });
    Ok((handle.into(), directory))
}

/// Draws the window and asserts what its observed element tree reported.
///
/// # Panics
///
/// When the window cannot be updated, or the shell or its pane area is not in
/// the drawn tree — the failing assertions this suite exists to make. The reasons live here and
/// not on the annotated function above, because that annotation moves the
/// enclosing function's documentation onto its generated wrapper.
fn assert_the_drawn_surface(context: &mut TestAppContext, window_handle: AnyWindowHandle) {
    let updated = context.update_window(window_handle, |_view, window, app_context| {
        window.draw(app_context).clear(app_context);
        assert!(window.find("window-shell").visible(), "the shell is drawn");
        assert!(window.find("pane-area").visible(), "the pane area is drawn");
    });
    assert!(
        updated.is_ok(),
        "the window could not be updated: {updated:?}"
    );
}
