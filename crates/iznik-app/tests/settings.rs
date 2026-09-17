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
