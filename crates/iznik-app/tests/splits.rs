//! Split interaction invariants.

use iznik_app::splits::{
    drag_command, drag_weights, equalize_command, equalized_weights, split_command,
};
use iznik_protocol::command::{Placement, SessionCommand};
use iznik_protocol::identity::{PaneId, TabId};
use iznik_protocol::model::{LayoutNode, SplitDirection, Weighted};

#[test]
/// A divider drag preserves total weight and a stationary drag is a no-op.
///
/// # Panics
///
/// Panics when the helper changes the total or a stationary drag.
fn divider_drag_preserves_weight() {
    let original = [2, 1, 3];
    let moved = drag_weights(&original, 0, 10.0, 100.0);
    assert_eq!(moved.iter().sum::<u32>(), original.iter().sum::<u32>());
    assert_eq!(drag_weights(&original, 0, 0.0, 100.0), original);
}

#[test]
/// Equalization gives every pane the same normalized factor.
///
/// # Panics
///
/// Panics when factors differ or an empty split is altered.
fn equalize_restores_shared_factors() {
    assert_eq!(equalized_weights(&[2, 5, 1]), vec![1, 1, 1]);
    assert!(equalized_weights(&[]).is_empty());
}

#[test]
/// A changed divider produces one normalized `SetLayout` command.
///
/// # Panics
///
/// Panics when the command is absent or does not preserve the requested tree.
fn divider_drag_builds_layout_command() {
    let layout = LayoutNode::Split {
        direction: SplitDirection::Horizontal,
        children: vec![
            Weighted {
                node: LayoutNode::Leaf(PaneId(1)),
                weight: 2,
            },
            Weighted {
                node: LayoutNode::Leaf(PaneId(2)),
                weight: 2,
            },
        ],
    };
    let command =
        drag_command(TabId(3), &layout, 0, 60.0, 100.0).expect("changed divider creates a command");
    let SessionCommand::SetLayout {
        tab,
        layout: updated_layout,
    } = command
    else {
        panic!("divider did not create SetLayout");
    };
    assert_eq!(tab, TabId(3));
    assert_eq!(updated_layout.leaves(), vec![PaneId(1), PaneId(2)]);
    assert_eq!(drag_command(TabId(3), &updated_layout, 0, 0.0, 100.0), None);
}

#[test]
/// Keyboard split and equalize operations use stable protocol identities.
///
/// # Panics
///
/// Panics when an operation names a different pane, tab or direction.
fn keyboard_split_commands_use_stable_identities() {
    assert_eq!(
        split_command(TabId(3), PaneId(1), SplitDirection::Vertical, true, 80, 24),
        SessionCommand::CreatePane {
            tab: TabId(3),
            placement: Placement {
                target: PaneId(1),
                direction: SplitDirection::Vertical,
                before: true,
            },
            columns: 80,
            rows: 24,
            working_directory: None,
        }
    );
    let layout = LayoutNode::Split {
        direction: SplitDirection::Horizontal,
        children: vec![
            Weighted {
                node: LayoutNode::Leaf(PaneId(1)),
                weight: 2,
            },
            Weighted {
                node: LayoutNode::Leaf(PaneId(2)),
                weight: 1,
            },
        ],
    };
    assert!(equalize_command(TabId(3), &layout).is_some());
}
