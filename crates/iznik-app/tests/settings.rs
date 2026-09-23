//! Settings and theme invariants.

use std::collections::BTreeMap;
use std::fs;

use iznik_app::settings::{Settings, Watcher, decode, encode, replace, validate_keybindings};
use iznik_app::theme::{AppTheme, terminal_theme};

#[path = "support/mod.rs"]
mod support;

#[test]
/// Theme conversion keeps emulator colors aligned with application colors.
///
/// # Panics
///
/// Panics when conversion changes either configured color.
fn theme_reaches_terminal_defaults() {
    let theme = AppTheme::default();
    let terminal = terminal_theme(&theme);
    assert_eq!(terminal.foreground, theme.foreground);
    assert_eq!(terminal.background, theme.background);
}

#[test]
/// Invalid keybinding updates preserve the previous settings.
///
/// # Panics
///
/// Panics when an invalid update is accepted or mutates the current value.
fn malformed_keybinding_is_refused() {
    let mut current = Settings::default();
    let previous = current.clone();
    let mut bindings = BTreeMap::new();
    bindings.insert(String::new(), "ctrl-x".to_owned());
    let candidate = Settings {
        keybindings: bindings,
        ..current.clone()
    };
    assert!(replace(&mut current, candidate).is_err());
    assert_eq!(current, previous);
}

#[test]
/// Settings encode and decode without losing theme or keybinding values.
///
/// # Panics
///
/// Panics when the stable settings format is not reversible.
fn settings_round_trip() {
    let mut settings = Settings::default();
    settings.theme.line_height = 1.5;
    settings.theme.tabs_in_title_bar = true;
    settings.theme_name = "Catppuccin Mocha".to_owned();
    settings
        .keybindings
        .insert("CreateSession".to_owned(), "ctrl-n".to_owned());
    assert_eq!(decode(&encode(&settings)).expect("round trip"), settings);
}

#[test]
/// Malformed fields are named by the decoder.
///
/// # Panics
///
/// Panics when malformed settings are accepted or do not name their field.
fn malformed_field_is_named() {
    let error = decode("font_size=not-a-number").expect_err("malformed field");
    assert_eq!(error.field, "font_size");
}

#[test]
/// Unknown actions and colliding chords are refused before application.
///
/// # Panics
///
/// Panics when invalid overrides pass validation.
fn overrides_validate_against_inventory() {
    let mut unknown = BTreeMap::new();
    unknown.insert("UnknownAction".to_owned(), "ctrl-x".to_owned());
    assert!(validate_keybindings(&unknown).is_err());
    let mut collision = BTreeMap::new();
    collision.insert("CreateSession".to_owned(), "ctrl-x".to_owned());
    collision.insert("CreateTab".to_owned(), "ctrl-x".to_owned());
    assert!(validate_keybindings(&collision).is_err());
}

#[test]
/// The watcher applies a changed file once and ignores an unchanged stamp.
///
/// # Panics
///
/// Panics when reload state does not match the file changes.
fn watcher_applies_changed_file() {
    let path = std::env::temp_dir().join("iznik-settings-watch");
    let settings = Settings::default();
    fs::write(&path, encode(&settings)).expect("settings fixture");
    let mut watcher = Watcher::new(&path);
    let mut current = Settings::default();
    assert!(watcher.reload(&mut current).expect("first reload"));
    assert!(!watcher.reload(&mut current).expect("unchanged reload"));
    fs::remove_file(path).expect("settings cleanup");
}

#[test]
/// A file that is not there yet is not a refusal, and it leaves the current
/// settings alone.
///
/// # Panics
///
/// Panics when a missing file is reported as an error or clears the current font.
fn a_missing_settings_file_keeps_the_current_value() {
    let path = std::env::temp_dir().join(format!(
        "iznik-settings-missing-{}-{}",
        std::process::id(),
        "keeps"
    ));
    let _removed = fs::remove_file(&path);
    let mut watcher = Watcher::new(&path);
    let mut current = Settings::default();
    current.theme.font_size = 19.0;
    assert!(
        !watcher.reload(&mut current).expect("missing file"),
        "a missing file changes nothing"
    );
    assert_eq!(
        current.theme.font_size.to_bits(),
        19.0_f32.to_bits(),
        "the current font size is kept"
    );
}

#[test]
/// A settings theme reaches a live emulator snapshot without restarting it.
///
/// # Panics
///
/// Panics when the VT owner retains a stale background color.
fn theme_change_reaches_live_emulator() {
    let thread =
        iznik_app::vt::VtThread::start(iznik_app::vt::VtOptions::default()).expect("VT thread");
    support::open(&thread, iznik_protocol::identity::Sequence(0), 80, 24).expect("open");
    let theme = AppTheme {
        background: libghostty_vt::style::RgbColor { r: 1, g: 2, b: 3 },
        ..AppTheme::default()
    };
    thread
        .send(iznik_app::vt::VtCommand::Theme {
            key: support::key(),
            theme: Box::new(terminal_theme(&theme)),
        })
        .expect("theme");
    let snapshot = support::snapshot(&thread).expect("snapshot");
    assert_eq!(snapshot.colors.background, theme.background);
}

#[path = "support/engine.rs"]
mod engine;

/// Font size distinct from the built-in default, so a forgotten file fails.
const SAVED_FONT_SIZE: f32 = 21.0;

/// Row height distinct from the built-in default.
const SAVED_LINE_HEIGHT: f32 = 1.5;

/// A bundled theme other than the default, so a relaunch that stays on Ayu fails.
const SAVED_THEME_NAME: &str = "Catppuccin Mocha";

#[gpui_kit::test]
fn a_changed_theme_is_what_the_next_shell_reads(context: &mut gpui_kit::TestAppContext) {
    check(&restores(context));
}

/// Keep fixture assertion outside the GPUI macro's generated test documentation.
///
/// # Panics
///
/// Fails on a fixture error.
fn check(result: &Result<(), Box<dyn std::error::Error>>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Change the theme through a shell, then open another shell on the same file.
///
/// # Errors
///
/// Returns a fixture, window, or settings-file failure.
///
/// # Panics
///
/// Panics when the second shell does not read the first shell's theme, font,
/// line height, or title-bar choice.
fn restores(context: &mut gpui_kit::TestAppContext) -> Result<(), Box<dyn std::error::Error>> {
    use std::rc::Rc;

    use gpui_kit::component::Theme;
    use iznik_app::theme::{apply_default_theme, apply_named};
    use iznik_app::vt::{VtOptions, VtThread};
    use iznik_app::window::{ShellOptions, WindowShell};

    context.update(|app| {
        gpui_kit::init(app);
        assert!(apply_default_theme(app).is_ok(), "bundled themes register");
    });
    let directory =
        std::env::temp_dir().join(format!("iznik-settings-persist-{}", std::process::id()));
    let _stale = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory)?;
    let path = directory.join("settings");
    let open = |window_context: &mut gpui_kit::TestAppContext, label: &str| {
        let (bridge, retained) = engine::start(label)?;
        let thread = Rc::new(VtThread::start(VtOptions::default())?);
        let handle = window_context.add_window(|window, build| {
            WindowShell::new(
                bridge,
                thread,
                ShellOptions {
                    update_interval: None,
                    settings_path: Some(path.clone()),
                    ..ShellOptions::default()
                },
                window,
                build,
            )
        });
        Ok::<_, Box<dyn std::error::Error>>((handle, retained))
    };
    let (first, _first_directory) = open(context, "settings-persist-first")?;
    context.update(|app| {
        assert!(
            apply_named(SAVED_THEME_NAME, app),
            "the saved theme is registered"
        );
    });
    first.update(context, |shell, _, app| {
        let mut theme = shell.settings().theme.clone();
        theme.font_size = SAVED_FONT_SIZE;
        theme.line_height = SAVED_LINE_HEIGHT;
        theme.tabs_in_title_bar = true;
        shell.set_theme(theme, app);
    })?;
    let saved = decode(&fs::read_to_string(&path)?).map_err(|error| format!("{error:?}"))?;
    assert_saved(&saved, "the written file");
    let (second, _second_directory) = open(context, "settings-persist-next")?;
    second.update(context, |shell, _, _app| {
        assert_saved(shell.settings(), "the next shell");
    })?;
    context.update(|app| {
        assert_eq!(
            Theme::global(app).theme_name().as_ref(),
            SAVED_THEME_NAME,
            "the next shell restores the kit theme, not only the terminal colors"
        );
    });
    let _removed = fs::remove_dir_all(&directory);
    Ok(())
}

/// Assert that `settings` carry the saved theme, font, line height and title-bar choice.
///
/// # Panics
///
/// Panics when any of them is not the saved value.
fn assert_saved(settings: &Settings, source: &str) {
    assert_eq!(
        settings.theme.font_size.to_bits(),
        SAVED_FONT_SIZE.to_bits(),
        "{source} keeps the font size"
    );
    assert_eq!(
        settings.theme.line_height.to_bits(),
        SAVED_LINE_HEIGHT.to_bits(),
        "{source} keeps the line height"
    );
    assert!(
        settings.theme.tabs_in_title_bar,
        "{source} keeps tabs in the title bar"
    );
    assert_eq!(
        settings.theme_name, SAVED_THEME_NAME,
        "{source} keeps the theme"
    );
}
