//! Behavioral checks for the command palette projection.

use iznik_app::actions::{ActionContext, ActionId, INVENTORY};
use iznik_app::host_ui::EngineState;
use iznik_app::palette::{Palette, PaletteAction, results, selected_action};
use iznik_client::host::identity::HostId;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::{MessageError, ToClient};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

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
    let filtered = results(&state, "begin");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "host: add");
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
        ..Palette::default()
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
        query: "begin".to_owned(),
        selected: 0,
        ..Palette::default()
    };
    assert_eq!(selected_action(&state, &palette), Some(ActionId::AddHost));
}

#[test]
/// A query matches the word it starts, not letters scattered through a sentence.
///
/// `settt` matches nothing: no command word starts with those letters.
/// Session, tab and pane commands stay listed for an empty query and drop
/// out, because "selected" and "tabs" are not that word.
///
/// # Panics
///
/// Panics when the filter keeps a command the query does not name, or drops
/// the settings command.
fn palette_word_prefix_filter() {
    let state = state_with_a_pane().expect("model encodes");
    let names = |query: &str| -> Vec<&str> {
        results(&state, query)
            .into_iter()
            .map(|specification| specification.name)
            .collect()
    };
    assert!(names("").iter().any(|name| name.starts_with("session:")));
    assert!(names("").iter().any(|name| name.starts_with("tab:")));
    assert!(names("").iter().any(|name| name.starts_with("pane:")));
    assert!(names("settt").is_empty());
    assert_eq!(names("sett"), ["iznik: settings"]);
    assert_eq!(names("set"), ["layout: set", "iznik: settings"]);
    assert!(names("sel").contains(&"session: rename"));
    assert!(!names("sel").contains(&"iznik: settings"));
}

/// A model with one session, tab and pane, so those commands are offered.
///
/// # Errors
///
/// Returns the model encoding error.
fn state_with_a_pane() -> Result<EngineState, MessageError> {
    let mut state = EngineState::new();
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "session".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "tab".to_owned(),
                panes: vec![Pane {
                    id: PaneId(1),
                    title: String::new(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
                layout: LayoutNode::Leaf(PaneId(1)),
            }],
        }],
    };
    state.apply(
        &HostId("devbox".to_owned()),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    );
    Ok(state)
}

#[test]
/// A key that produced several characters adds all of them, and pasted text
/// becomes one line.
///
/// # Panics
///
/// Panics when characters are dropped or a line break reaches the query.
fn palette_takes_whole_key_text_and_a_paste() {
    let mut palette = Palette::default();
    palette.open();
    assert_eq!(
        palette.key_text("", Some("\u{e9}t\u{e9}"), 1),
        PaletteAction::Ignored
    );
    assert_eq!(palette.query, "\u{e9}t\u{e9}", "every composed character");
    palette.insert(" web\nhost\u{7}");
    assert_eq!(
        palette.query, "\u{e9}t\u{e9} web host",
        "one line, no controls"
    );
}
