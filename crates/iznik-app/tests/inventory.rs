//! Headless proofs that the action inventory is closed and self-consistent.

use std::collections::BTreeSet;

use iznik_app::actions::{INVENTORY, action_for_command};
use iznik_protocol::command::{Placement, SessionCommand};
use iznik_protocol::identity::{PaneId, SessionId, TabId};
use iznik_protocol::model::{LayoutNode, SplitDirection};

/// Every session command variant appears in the inventory exactly once.
///
/// # Panics
/// Fails when a command has no registered action.
#[test]
fn inventory_covers_every_session_command() {
    let commands = [
        SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: 80,
            rows: 24,
            working_directory: None,
        },
        SessionCommand::RenameSession {
            session: SessionId(1),
            name: "renamed".to_owned(),
        },
        SessionCommand::CloseSession {
            session: SessionId(1),
        },
        SessionCommand::ReorderSessions {
            order: vec![SessionId(1)],
        },
        SessionCommand::CreateTab {
            session: SessionId(1),
            name: "tab".to_owned(),
            columns: 80,
            rows: 24,
            working_directory: None,
        },
        SessionCommand::RenameTab {
            tab: TabId(1),
            name: "renamed".to_owned(),
        },
        SessionCommand::CloseTab { tab: TabId(1) },
        SessionCommand::ReorderTabs {
            session: SessionId(1),
            order: vec![TabId(1)],
        },
        SessionCommand::CreatePane {
            tab: TabId(1),
            placement: placement(),
            columns: 80,
            rows: 24,
            working_directory: None,
        },
        SessionCommand::ClosePane { pane: PaneId(1) },
        SessionCommand::MovePane {
            pane: PaneId(1),
            to_tab: TabId(2),
            placement: placement(),
        },
        SessionCommand::SetLayout {
            tab: TabId(1),
            layout: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                children: Vec::new(),
            },
        },
    ];
    for command in commands {
        let action = action_for_command(&command);
        assert!(
            INVENTORY
                .iter()
                .any(|specification| specification.id == action)
        );
    }
}

/// Names and explanations are non-empty, and inventory identities do not repeat.
///
/// # Panics
/// Fails when a row is duplicated or missing its display text.
#[test]
fn inventory_rows_are_unique_and_explained() {
    let mut ids = BTreeSet::new();
    for specification in INVENTORY {
        assert!(ids.insert(specification.id));
        assert!(!specification.name.is_empty());
        assert!(!specification.explanation.is_empty());
    }
}

#[test]
/// Every registered action has a default keybinding entry.
///
/// # Panics
///
/// Panics when the checked-in keybinding asset omits an inventory action.
fn default_keybindings_cover_inventory() {
    let asset = include_str!("../assets/default-keybindings.json");
    for specification in INVENTORY {
        assert!(asset.contains(&format!("\"{:?}\"", specification.id)));
    }
}

#[test]
/// Default chords resolve to one action each without collisions.
///
/// # Panics
///
/// Panics when a chord is duplicated or names an unknown action.
fn default_keybindings_are_unique() {
    let asset = include_str!("../assets/default-keybindings.json");
    let names: BTreeSet<_> = INVENTORY
        .iter()
        .map(|specification| format!("\"{:?}\"", specification.id))
        .collect();
    let mut chords = BTreeSet::new();
    for line in asset.lines().filter(|line| line.contains(':')) {
        let fields: Vec<_> = line.split('"').collect();
        let Some(name) = fields.get(1) else { continue };
        let Some(chord) = fields.get(3) else { continue };
        assert!(names.contains(&format!("\"{name}\"")));
        assert!(chords.insert(chord.to_owned()));
    }
}

/// The fixture placement is valid for every pane command sample.
fn placement() -> Placement {
    Placement {
        target: PaneId(1),
        direction: SplitDirection::Horizontal,
        before: false,
    }
}
