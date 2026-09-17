//! Keyboard-driven opening and closing of the command palette overlay.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext};
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

/// Choosing Add Host turns the palette into its alias prompt instead of
/// sending anything.
#[gpui_kit::test]
fn add_host_asks_for_its_alias(context: &mut TestAppContext) {
    check(&add_host_prompt(context));
}

/// A default chord for an argument action opens the palette at its prompt.
#[gpui_kit::test]
fn a_chord_opens_its_prompt(context: &mut TestAppContext) {
    check(&chord_prompt(context));
}

/// A default chord whose action has nothing to act on says so in a notification.
#[gpui_kit::test]
fn a_chord_with_nothing_to_act_on_shows_a_notice(context: &mut TestAppContext) {
    check(&chord_notice(context));
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

/// Open a shell inside the kit's root, which hosts the notification layer.
///
/// # Errors
/// Returns setup failures.
fn open_in_root<'context>(
    context: &'context mut TestAppContext,
    label: &str,
) -> Result<(&'context mut gpui_kit::VisualTestContext, PathBuf), Failed> {
    context.update(gpui_kit::init);
    let directory = temporary_directory(label)?;
    let (bridge, thread) = owners(&directory)?;
    let (_view, context) = context.add_window_view(|window, context| {
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
        gpui_kit::component::Root::new(shell, window, context)
    });
    Ok((context, directory))
}

/// Select Add Host with Enter and assert its prompt appears and nothing is sent.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when the prompt does not replace the inventory or a notice appears.
fn add_host_prompt(context: &mut TestAppContext) -> Result<(), Failed> {
    let (context, directory) = open_in_root(context, "add-host")?;
    context.simulate_keystrokes("ctrl-shift-p");
    context.simulate_keystrokes("enter");
    context.update(|window, _application| {
        window.find("command-palette-question").visible();
        assert!(
            window.try_find("palette-AddHost").is_none(),
            "the prompt replaces the inventory rows"
        );
        assert!(
            window.try_find("notification").is_none(),
            "asking for an argument is not a failure"
        );
    });
    context.simulate_keystrokes("enter");
    context.update(|window, _application| {
        window.find("command-palette-question").visible();
    });
    context.simulate_keystrokes("escape");
    context.update(|window, _application| {
        assert!(
            window.try_find("command-palette-question").is_none(),
            "escape abandons the prompt"
        );
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Press the Add Host chord with the palette closed and assert its prompt opens.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when the chord does not open the prompt.
fn chord_prompt(context: &mut TestAppContext) -> Result<(), Failed> {
    let (context, directory) = open_in_root(context, "chord-prompt")?;
    context.simulate_keystrokes("ctrl-shift-h");
    context.update(|window, _application| {
        window.find("command-palette-question").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Press the New Session chord with no host and assert a notice, with the palette closed.
///
/// # Errors
/// Returns setup or assertion failures.
///
/// # Panics
/// Panics when no notice appears or the palette is left open.
fn chord_notice(context: &mut TestAppContext) -> Result<(), Failed> {
    let (context, directory) = open_in_root(context, "chord-notice")?;
    context.simulate_keystrokes("ctrl-shift-n");
    context.update(|window, _application| {
        window.find("notification").visible();
        assert!(
            window.try_find("command-palette-query").is_none(),
            "a chord that sent nothing leaves the palette closed"
        );
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}
