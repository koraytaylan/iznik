//! Option chords that a keyboard layout turns into characters stay characters.

use gpui_kit::{Keystroke, Modifiers};
use iznik_app::grid::keyboard::keyboard;
use libghostty_vt::key::{Action, Key, Mods};

/// A keystroke with the modifiers a layout test holds.
fn stroke(key: &str, character: Option<&str>, modifiers: Modifiers) -> Keystroke {
    Keystroke {
        modifiers,
        key: key.to_owned(),
        key_char: character.map(str::to_owned),
    }
}

/// Turkish Q Option+Q is the character `@`, with no Alt chord left on it.
///
/// # Panics
/// Panics when the mapped event is not that character.
#[test]
fn option_at_is_layout_text() {
    let input = keyboard(
        &stroke(
            "q",
            Some("@"),
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
        Action::Press,
    );
    assert_eq!(input.key, Key::Unidentified);
    assert_eq!(input.text, "@");
    assert_eq!(input.unshifted, None);
    assert_eq!(input.modifiers, Mods::empty());
}

/// Option+Shift+Q is `Œ`, including when Control arrives with Alt.
///
/// # Panics
/// Panics when the mapped event is not that character.
#[test]
fn option_shift_is_layout_character() {
    let input = keyboard(
        &stroke(
            "q",
            Some("\u{152}"),
            Modifiers {
                alt: true,
                shift: true,
                control: true,
                ..Modifiers::default()
            },
        ),
        Action::Press,
    );
    assert_eq!(input.key, Key::Unidentified);
    assert_eq!(input.text, "\u{152}");
    assert_eq!(input.modifiers, Mods::empty());
}

/// Alt held on the unmodified letter stays a chord.
///
/// # Panics
/// Panics when the letter is rewritten as layout text.
#[test]
fn letter_chord_keeps_modifiers() {
    let input = keyboard(
        &stroke(
            "q",
            None,
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
        Action::Press,
    );
    assert_eq!(input.key, Key::Q);
    assert_eq!(input.text, "q");
    assert_eq!(input.modifiers, Mods::ALT);
}

/// A symbol-named key still carries the layout character without Alt.
///
/// # Panics
/// Panics when the symbol is dropped or kept as a modifier chord.
#[test]
fn symbol_name_keeps_layout_text() {
    let input = keyboard(
        &stroke(
            "@",
            Some("@"),
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
        Action::Press,
    );
    assert_eq!(input.key, Key::Unidentified);
    assert_eq!(input.text, "@");
    assert_eq!(input.modifiers, Mods::empty());
}

/// With Option as Meta, Option+Q is Alt+Q whatever the layout puts there.
///
/// # Panics
/// Panics when the layout character wins over the Meta chord.
#[test]
fn option_as_meta_sends_the_chord() {
    let input = iznik_app::grid::keyboard::keyboard_as(
        &stroke(
            "q",
            Some("@"),
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        ),
        Action::Press,
        true,
    );
    assert_eq!(input.key, Key::Q);
    assert_eq!(input.text, "q");
    assert_eq!(input.modifiers, Mods::ALT);
}
