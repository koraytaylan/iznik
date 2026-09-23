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
/// The history budget defaults to megabytes, round-trips, and is clamped.
///
/// # Panics
///
/// Panics when the default is not the named budget, the value is lost, or an
/// out-of-range value is not clamped.
fn scrollback_budget_defaults_round_trips_and_is_clamped() {
    use iznik_app::vt::{MAXIMUM_SCROLLBACK_BYTES, MINIMUM_SCROLLBACK_BYTES, SCROLLBACK_BYTES};
    assert_eq!(Settings::default().scrollback_bytes, SCROLLBACK_BYTES);
    let settings = Settings {
        scrollback_bytes: 2_097_152,
        ..Settings::default()
    };
    assert_eq!(decode(&encode(&settings)).expect("round trip"), settings);
    let small = decode("scrollback_bytes=1").expect("small");
    assert_eq!(small.scrollback_bytes, MINIMUM_SCROLLBACK_BYTES);
    let large = decode("scrollback_bytes=99999999999999").expect("large");
    assert_eq!(large.scrollback_bytes, MAXIMUM_SCROLLBACK_BYTES);
    assert_eq!(
        decode("scrollback_bytes=lots").expect_err("words").field,
        "scrollback_bytes"
    );
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
    let path = iznik_testkit::scratch::path("settings-watch");
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
    let path = iznik_testkit::scratch::path("settings-missing");
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
/// line height, title-bar choice, or program clipboard choice.
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
    let directory = iznik_testkit::scratch::path("settings-persist");
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
        shell.edit_behavior(|behavior| behavior.clipboard_write = false, app);
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
    assert!(
        !settings.clipboard_write,
        "{source} keeps the program clipboard turned off"
    );
}

#[test]
/// A field this version does not know is kept, not refused, and a font size
/// the grid cannot draw is brought into range.
///
/// # Panics
///
/// Panics when an unknown field refuses the file, is lost on encoding, or a
/// zero or non-finite size is accepted as it is.
fn unknown_fields_are_kept_and_sizes_bounded() {
    let settings = decode("# mine\nfuture_option=on\nfont_size=0\n").expect("decodes");
    assert_eq!(settings.unknown_fields(), ["future_option"]);
    assert!(
        settings.theme.font_size > 0.0,
        "zero is raised to the minimum"
    );
    let written = encode(&settings);
    assert!(written.contains("# mine\n"), "comment kept: {written}");
    assert!(
        written.contains("future_option=on\n"),
        "field kept: {written}"
    );
    assert_eq!(decode("font_size=NaN").expect_err("NaN").field, "font_size");
    let theme = AppTheme {
        font_size: f32::NAN,
        line_height: 0.0,
        ..AppTheme::default()
    };
    let drawable = iznik_app::settings::drawable(&theme);
    assert!(drawable.font_size.is_finite() && drawable.font_size > 0.0);
    assert!(drawable.line_height > 0.0);
}

#[test]
/// A refused file is refused once, not on every poll; writing keeps lines
/// another version added since the file was read.
///
/// # Panics
///
/// Panics when a refused file is refused again unchanged, or a write drops a
/// line it does not own.
fn refused_once_and_foreign_lines_survive_a_write() {
    let directory =
        std::env::temp_dir().join(format!("iznik-settings-robust-{}", std::process::id()));
    let _stale = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("settings");
    fs::write(&path, "font_size=big\n").expect("bad file");
    let mut watcher = Watcher::new(&path);
    let mut current = Settings::default();
    assert!(watcher.reload(&mut current).is_err(), "refused");
    assert!(
        matches!(watcher.reload(&mut current), Ok(false)),
        "not refused again until it changes"
    );
    fs::write(&path, "newer_setting=1\n").expect("foreign line");
    watcher.write(&Settings::default()).expect("write");
    let text = fs::read_to_string(&path).expect("read");
    assert!(text.contains("newer_setting=1"), "kept: {text}");
    assert!(text.contains("font_size="), "owned fields written: {text}");
    let leftovers = fs::read_dir(&directory).expect("list").count();
    assert_eq!(leftovers, 1, "no temporary file is left behind");
    let _removed = fs::remove_dir_all(&directory);
}
