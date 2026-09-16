//! Pure split interaction helpers used by the window chrome.

use gpui_kit::{AnyElement, IntoElement};
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{PaneId, TabId};
use iznik_protocol::model::{LayoutNode, SplitDirection, Weighted};

/// The smallest positive factor accepted by the normalized layout.
const MINIMUM_WEIGHT: u32 = 1;
/// Half an extent is the smallest drag considered a full weight transfer.
const HALF_EXTENT: f32 = 2.0;

/// Return weights after dragging the divider at `divider` by `delta` pixels.
///
/// The total weight is preserved and a drag that cannot change both adjacent
/// panes returns the original values.
#[must_use]
pub fn drag_weights(weights: &[u32], divider: usize, delta: f32, extent: f32) -> Vec<u32> {
    let Some((left, right)) = divider
        .checked_add(1)
        .and_then(|index| weights.get(divider).zip(weights.get(index)))
    else {
        return weights.to_vec();
    };
    if extent <= 0.0 || !delta.is_finite() {
        return weights.to_vec();
    }
    let change = if delta > extent / HALF_EXTENT {
        1_i64
    } else if delta < -extent / HALF_EXTENT {
        -1_i64
    } else {
        0_i64
    };
    let left_value = i64::from(*left).saturating_add(change);
    let right_value = i64::from(*right).saturating_sub(change);
    if left_value < i64::from(MINIMUM_WEIGHT) || right_value < i64::from(MINIMUM_WEIGHT) {
        return weights.to_vec();
    }
    let mut result = weights.to_vec();
    let Some(split_index) = divider.checked_add(1) else {
        return weights.to_vec();
    };
    let (before, after) = result.split_at_mut(split_index);
    let Some(left_slot) = before.get_mut(divider) else {
        return weights.to_vec();
    };
    let Some(right_slot) = after.first_mut() else {
        return weights.to_vec();
    };
    let Ok(left_weight) = u32::try_from(left_value) else {
        return weights.to_vec();
    };
    let Ok(right_weight) = u32::try_from(right_value) else {
        return weights.to_vec();
    };
    *left_slot = left_weight;
    *right_slot = right_weight;
    result
}

/// Equalize every child weight while retaining the number of children.
#[must_use]
pub fn equalized_weights(weights: &[u32]) -> Vec<u32> {
    if weights.is_empty() {
        return Vec::new();
    }
    vec![MINIMUM_WEIGHT; weights.len()]
}

/// Build the authoritative layout command for a changed root divider.
#[must_use]
pub fn drag_command(
    tab: TabId,
    layout: &LayoutNode,
    divider: usize,
    delta: f32,
    extent: f32,
) -> Option<SessionCommand> {
    let LayoutNode::Split {
        direction,
        children,
    } = layout
    else {
        return None;
    };
    let weights: Vec<_> = children.iter().map(|child| child.weight).collect();
    let moved = drag_weights(&weights, divider, delta, extent);
    if moved == weights {
        return None;
    }
    let updated = children
        .iter()
        .zip(moved)
        .map(|(child, weight)| Weighted {
            node: child.node.clone(),
            weight,
        })
        .collect();
    Some(SessionCommand::SetLayout {
        tab,
        layout: LayoutNode::Split {
            direction: *direction,
            children: updated,
        }
        .normalize(),
    })
}

/// Build a layout command that equalizes the children of the root split.
#[must_use]
pub fn equalize_command(tab: TabId, layout: &LayoutNode) -> Option<SessionCommand> {
    let LayoutNode::Split {
        direction,
        children,
    } = layout
    else {
        return None;
    };
    let weights: Vec<_> = children.iter().map(|child| child.weight).collect();
    let equalized = equalized_weights(&weights);
    if equalized == weights {
        return None;
    }
    let updated = children
        .iter()
        .zip(equalized)
        .map(|(child, weight)| Weighted {
            node: child.node.clone(),
            weight,
        })
        .collect();
    Some(SessionCommand::SetLayout {
        tab,
        layout: LayoutNode::Split {
            direction: *direction,
            children: updated,
        },
    })
}

/// Build the command that places a new pane beside an existing pane.
#[must_use]
pub fn split_command(
    tab: TabId,
    target: PaneId,
    direction: SplitDirection,
    before: bool,
    columns: u16,
    rows: u16,
) -> SessionCommand {
    SessionCommand::CreatePane {
        tab,
        placement: iznik_protocol::command::Placement {
            target,
            direction,
            before,
        },
        columns,
        rows,
        working_directory: None,
    }
}

/// Render the authoritative layout tree through the existing layout renderer.
#[must_use]
pub fn render(
    layout: &LayoutNode,
    revision: u64,
    pane: impl Fn(PaneId) -> AnyElement,
) -> AnyElement {
    crate::layout::render_layout(layout, revision, pane).into_any_element()
}

/// Render the layout with the native divider resize callback attached.
#[must_use]
pub fn render_interactive(
    layout: &LayoutNode,
    revision: u64,
    pane: impl Fn(PaneId) -> AnyElement,
    on_resize: crate::layout::ResizeCallback,
) -> AnyElement {
    crate::layout::render_layout_with_resize(layout, revision, pane, on_resize).into_any_element()
}
