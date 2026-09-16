//! Preserve native layout state while mapping keys for the terminal encoder.

use crate::input::KeyInput;
use gpui_kit::{Keystroke, Modifiers};
use libghostty_vt::key::{Action, Key, Mods};

/// GPUI's documented normalized names map directly to native key identities.
/// Letters and digits retain their logical identity; no keypad location is inferred.
const NAMED_KEYS: &[(&str, Key)] = &[
    ("numpad0", Key::Numpad0),
    ("numpad1", Key::Numpad1),
    ("numpad2", Key::Numpad2),
    ("numpad3", Key::Numpad3),
    ("numpad4", Key::Numpad4),
    ("numpad5", Key::Numpad5),
    ("numpad6", Key::Numpad6),
    ("numpad7", Key::Numpad7),
    ("numpad8", Key::Numpad8),
    ("numpad9", Key::Numpad9),
    ("numpadadd", Key::NumpadAdd),
    ("numpadbackspace", Key::NumpadBackspace),
    ("numpadclear", Key::NumpadClear),
    ("numpadclearentry", Key::NumpadClearEntry),
    ("numpadcomma", Key::NumpadComma),
    ("numpaddecimal", Key::NumpadDecimal),
    ("numpaddivide", Key::NumpadDivide),
    ("numpadenter", Key::NumpadEnter),
    ("numpadequal", Key::NumpadEqual),
    ("numpadmultiply", Key::NumpadMultiply),
    ("numpadsubtract", Key::NumpadSubtract),
    ("numpadseparator", Key::NumpadSeparator),
    ("numpadup", Key::NumpadUp),
    ("numpaddown", Key::NumpadDown),
    ("numpadleft", Key::NumpadLeft),
    ("numpadright", Key::NumpadRight),
    ("numpadbegin", Key::NumpadBegin),
    ("numpadhome", Key::NumpadHome),
    ("numpadend", Key::NumpadEnd),
    ("numpadinsert", Key::NumpadInsert),
    ("numpaddelete", Key::NumpadDelete),
    ("numpadpageup", Key::NumpadPageUp),
    ("numpadpagedown", Key::NumpadPageDown),
    ("backspace", Key::Backspace),
    ("enter", Key::Enter),
    ("escape", Key::Escape),
    ("tab", Key::Tab),
    ("space", Key::Space),
    ("delete", Key::Delete),
    ("insert", Key::Insert),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("up", Key::ArrowUp),
    ("down", Key::ArrowDown),
    ("left", Key::ArrowLeft),
    ("right", Key::ArrowRight),
    ("f1", Key::F1),
    ("f2", Key::F2),
    ("f3", Key::F3),
    ("f4", Key::F4),
    ("f5", Key::F5),
    ("f6", Key::F6),
    ("f7", Key::F7),
    ("f8", Key::F8),
    ("f9", Key::F9),
    ("f10", Key::F10),
    ("f11", Key::F11),
    ("f12", Key::F12),
    ("f13", Key::F13),
    ("f14", Key::F14),
    ("f15", Key::F15),
    ("f16", Key::F16),
    ("f17", Key::F17),
    ("f18", Key::F18),
    ("f19", Key::F19),
    ("f20", Key::F20),
    ("f21", Key::F21),
    ("f22", Key::F22),
    ("f23", Key::F23),
    ("f24", Key::F24),
    ("f25", Key::F25),
    ("a", Key::A),
    ("b", Key::B),
    ("c", Key::C),
    ("d", Key::D),
    ("e", Key::E),
    ("f", Key::F),
    ("g", Key::G),
    ("h", Key::H),
    ("i", Key::I),
    ("j", Key::J),
    ("k", Key::K),
    ("l", Key::L),
    ("m", Key::M),
    ("n", Key::N),
    ("o", Key::O),
    ("p", Key::P),
    ("q", Key::Q),
    ("r", Key::R),
    ("s", Key::S),
    ("t", Key::T),
    ("u", Key::U),
    ("v", Key::V),
    ("w", Key::W),
    ("x", Key::X),
    ("y", Key::Y),
    ("z", Key::Z),
    ("0", Key::Digit0),
    ("1", Key::Digit1),
    ("2", Key::Digit2),
    ("3", Key::Digit3),
    ("4", Key::Digit4),
    ("5", Key::Digit5),
    ("6", Key::Digit6),
    ("7", Key::Digit7),
    ("8", Key::Digit8),
    ("9", Key::Digit9),
    ("`", Key::Backquote),
    ("\\", Key::Backslash),
    ("[", Key::BracketLeft),
    ("]", Key::BracketRight),
    (",", Key::Comma),
    ("=", Key::Equal),
    ("-", Key::Minus),
    (".", Key::Period),
    ("'", Key::Quote),
    (";", Key::Semicolon),
    ("/", Key::Slash),
];

/// Preserve every keyboard modifier exposed by GPUI that the native protocol supports.
pub(super) fn modifiers(value: Modifiers) -> Mods {
    let mut result = Mods::empty();
    result.set(Mods::SHIFT, value.shift);
    result.set(Mods::CTRL, value.control);
    result.set(Mods::ALT, value.alt);
    result.set(Mods::SUPER, value.platform);
    result
}

/// Preserve layout text; control chords may omit text, so recover an ASCII letter
/// from GPUI's normalized name. Unknown layout text stays intact without guessing
/// a physical key or an unshifted symbol the framework did not provide.
pub(super) fn keyboard(stroke: &Keystroke, action: Action) -> KeyInput {
    let key = NAMED_KEYS
        .iter()
        .find_map(|(name, key)| (*name == stroke.key).then_some(*key))
        .unwrap_or(Key::Unidentified);
    let mut characters = stroke.key.chars();
    let first = characters.next();
    let unshifted = first.filter(|_| characters.next().is_none());
    let text = stroke.key_char.clone().unwrap_or_else(|| {
        if stroke.key == "space" {
            return " ".into();
        }
        unshifted.map_or_else(String::new, |character| {
            if stroke.modifiers.shift && character.is_ascii_lowercase() {
                character.to_ascii_uppercase().to_string()
            } else {
                character.to_string()
            }
        })
    });
    KeyInput {
        key,
        action,
        modifiers: modifiers(stroke.modifiers),
        consumed: Mods::empty(),
        text,
        unshifted,
    }
}
