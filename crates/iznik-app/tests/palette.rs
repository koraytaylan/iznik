//! Behavioral checks for the command palette projection.

use iznik_app::actions::{ActionContext, ActionId, INVENTORY};
use iznik_app::host_ui::EngineState;
use iznik_app::palette::{Palette, PaletteAction, results, selected_action};

#[test]
/// Available entries and fuzzy filtering stay aligned with the inventory.
///
/// # Panics
///
/// Panics when filtering or availability differs from the expected fixture.
fn palette_lists_only_available_entries_and_fuzzy_filters() {
    let state = EngineState::new();
    let all = results(&state, "");
    assert_eq!(
        all.len(),
        INVENTORY
            .iter()
            .filter(|specification| matches!(specification.context, ActionContext::None))
            .count()
    );
    let filtered = results(&state, "add h");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "Add Host");
}

#[test]
/// Selection wraps and opening clears transient palette state.
///
/// # Panics
///
/// Panics when selection state does not follow the palette rules.
fn palette_selection_wraps_and_open_resets_state() {
    let mut palette = Palette {
        open: true,
        query: "old".to_owned(),
        selected: 2,
    };
    palette.move_selection(true, 3);
    assert_eq!(palette.selected, 0);
    palette.move_selection(false, 3);
    assert_eq!(palette.selected, 2);
    palette.open();
    assert!(palette.open);
    assert!(palette.query.is_empty());
    assert_eq!(palette.selected, 0);
}

#[test]
/// Normalized palette keys update query, selection, and lifecycle actions.
///
/// # Panics
///
/// Panics when a normalized key produces the wrong palette action.
fn palette_keys_drive_state() {
    let mut palette = Palette::default();
    palette.open();
    assert_eq!(palette.key("", Some('a'), 2), PaletteAction::Ignored);
    assert_eq!(palette.query, "a");
    assert_eq!(palette.key("down", None, 2), PaletteAction::Ignored);
    assert_eq!(palette.selected, 1);
    assert_eq!(palette.key("enter", None, 2), PaletteAction::Dispatch);
    assert_eq!(palette.key("escape", None, 2), PaletteAction::Closed);
    assert!(!palette.open);
}

#[test]
/// Backspace removes the last typed character and resets the selection.
///
/// # Panics
///
/// Panics when backspace does not shrink the query by one character.
fn palette_backspace_removes_the_last_character() {
    let mut palette = Palette::default();
    palette.open();
    assert_eq!(palette.key("", Some('a'), 1), PaletteAction::Ignored);
    assert_eq!(palette.key("", Some('d'), 1), PaletteAction::Ignored);
    assert_eq!(palette.query, "ad");
    assert_eq!(palette.key("backspace", None, 1), PaletteAction::Ignored);
    assert_eq!(palette.query, "a");
    assert_eq!(palette.key("backspace", None, 1), PaletteAction::Ignored);
    assert_eq!(palette.query, "");
    assert_eq!(palette.key("backspace", None, 1), PaletteAction::Ignored);
    assert_eq!(palette.query, "");
}

#[test]
/// Palette selection resolves the same inventory identity the shell dispatches.
///
/// # Panics
///
/// Panics when fuzzy filtering selects a different action identity.
fn palette_selection_resolves_inventory_identity() {
    let state = EngineState::new();
    let palette = Palette {
        open: true,
        query: "add h".to_owned(),
        selected: 0,
    };
    assert_eq!(selected_action(&state, &palette), Some(ActionId::AddHost));
}
