//! Opening the settings window from the palette.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, TestAppContext, VisualContext, VisualTestContext};
use iznik_app::bridge::EngineBridge;
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::transport::ClientRuntimePaths;

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Choosing "iznik: settings" opens a second window over the shell's theme.
#[gpui_kit::test]
fn settings_action_opens_a_window(context: &mut TestAppContext) {
    check(&opens_settings_window(context));
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

/// Allocate an isolated directory for one fixture's engine paths.
///
/// # Errors
/// Returns filesystem errors.
fn temporary_directory(label: &str) -> Result<PathBuf, Failed> {
    let directory = std::env::temp_dir().join(format!(
        "iznik-app-settings-window-{label}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&directory).ok();
    std::fs::create_dir_all(directory.join("artifacts"))?;
    Ok(directory)
}

/// Type "settings" through the palette's `key->key` keystroke syntax, which
/// uniquely fuzzy-matches "iznik: settings" among the always-available rows.
const TYPE_SETTINGS: &str = "s->s e->e t->t t->t i->i n->n g->g s->s";

/// Return the window opened besides `main`, once the palette dispatch runs.
///
/// # Errors
/// Returns a failure when dispatch did not open exactly one additional window.
///
/// # Panics
/// Panics when more or fewer than two windows are open.
fn opened_window(
    context: &TestAppContext,
    main: AnyWindowHandle,
) -> Result<AnyWindowHandle, Failed> {
    let windows = context.windows();
    assert_eq!(windows.len(), 2, "dispatch must open exactly one window");
    windows
        .into_iter()
        .find(|window| *window != main)
        .ok_or_else(|| "no window besides the main one".into())
}

/// Drive the shortcut and open-settings query, and assert a second window
/// renders the settings panel's content.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when the settings window does not open or render its content.
fn opens_settings_window(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory("open")?;
    let (bridge, thread) = owners(&directory)?;
    let (_view, main) = context.add_window_view(|window, context| {
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
    main.simulate_keystrokes("ctrl-shift-p");
    main.simulate_keystrokes(TYPE_SETTINGS);
    main.simulate_keystrokes("enter");
    main.update(|window, _application| {
        assert!(
            window.try_find("command-palette-panel").is_none(),
            "choosing the settings row must close the palette"
        );
    });
    let settings_handle = opened_window(main, main_handle)?;
    let mut settings = VisualTestContext::from_window(settings_handle, main);
    settings.update(|window, application| {
        window.render_frame(application);
        window.find("settings-window-content").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}
