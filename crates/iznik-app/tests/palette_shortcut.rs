//! Keyboard-driven opening and closing of the command palette overlay.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt;
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

/// `ctrl-shift-p` opens the palette showing the always-available inventory,
/// with no host or session needed.
#[gpui_kit::test]
fn the_shortcut_opens_the_palette(context: &mut TestAppContext) {
    check(&opens(context));
}

/// Escape closes a palette that the shortcut opened.
#[gpui_kit::test]
fn escape_closes_the_open_palette(context: &mut TestAppContext) {
    check(&closes(context));
}

/// Typed characters filter the inventory; backspace undoes them one at a time.
#[gpui_kit::test]
fn typing_filters_and_backspace_restores_the_palette(context: &mut TestAppContext) {
    check(&types_and_deletes(context));
}

/// Choosing Add Host before its host-entry form exists surfaces a failure
/// notice instead of silently doing nothing.
#[gpui_kit::test]
fn add_host_without_a_form_shows_a_failure_notice(context: &mut TestAppContext) {
    check(&add_host_notice(context));
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
    let directory =
        std::env::temp_dir().join(format!("iznik-app-palette-{label}-{}", std::process::id()));
    std::fs::remove_dir_all(&directory).ok();
    std::fs::create_dir_all(directory.join("artifacts"))?;
    Ok(directory)
}

/// Drive the shortcut and assert the palette opened with its inventory row.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when the shortcut does not open the palette or list its row.
fn opens(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory("open")?;
    let (bridge, thread) = owners(&directory)?;
    let (_view, context) = context.add_window_view(|window, context| {
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
    context.simulate_keystrokes("ctrl-shift-p");
    context.update(|window, _application| {
        window.find("command-palette").visible();
        window.find("palette-AddHost").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Drive the shortcut, then Escape, and assert the overlay is gone.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when Escape does not clear the palette's rendered inventory.
fn closes(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory("close")?;
    let (bridge, thread) = owners(&directory)?;
    let (_view, context) = context.add_window_view(|window, context| {
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
    context.simulate_keystrokes("ctrl-shift-p");
    context.simulate_keystrokes("escape");
    context.update(|window, _application| {
        assert!(
            window.try_find("palette-AddHost").is_none(),
            "escape must close the palette overlay"
        );
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Type a non-matching query, then delete it back to empty with backspace.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when typing does not filter the inventory or backspace does not restore it.
fn types_and_deletes(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory("type")?;
    let (bridge, thread) = owners(&directory)?;
    let (_view, context) = context.add_window_view(|window, context| {
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
    context.simulate_keystrokes("ctrl-shift-p");
    context.simulate_keystrokes("z->z z->z z->z");
    context.update(|window, _application| {
        assert!(
            window.try_find("palette-AddHost").is_none(),
            "a query matching no entry must filter it out"
        );
    });
    context.simulate_keystrokes("backspace backspace backspace");
    context.update(|window, _application| {
        window.find("palette-AddHost").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Select Add Host with Enter and assert the failure banner appears.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when the failure banner does not appear.
fn add_host_notice(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory("add-host")?;
    let (bridge, thread) = owners(&directory)?;
    let (_view, context) = context.add_window_view(|window, context| {
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
    context.simulate_keystrokes("ctrl-shift-p");
    context.simulate_keystrokes("enter");
    context.update(|window, _application| {
        window.find("surface-failure").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}
