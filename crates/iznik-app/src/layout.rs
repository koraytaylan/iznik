//! Translate the model's weighted tree into kit-owned resizable panels.

use gpui_kit::component::resizable::{ResizablePanelGroup, resizable_panel};
use std::rc::Rc;

use gpui_kit::component::resizable::ResizableState;
use gpui_kit::{
    AnyElement, App, Axis, Entity, IntoElement, ParentElement, Pixels, SharedString, Styled,
    Window, px,
};
use iznik_protocol::identity::PaneId;
use iznik_protocol::model::{LayoutNode, SplitDirection};

/// Callback invoked after GPUI changes a resizable panel group.
pub type ResizeCallback = Rc<dyn Fn(&Entity<ResizableState>, &mut Window, &mut App)>;

/// Render one authoritative layout while reusing the caller's pane entities.
///
/// `revision` changes only when the layout tree changes, so kit divider state
/// survives ordinary paint and window resizing but cannot override new model
/// weights. The caller supplies each leaf's existing entity as an element;
/// this function creates no replacement terminal surfaces.
pub fn render_layout(
    layout: &LayoutNode,
    revision: u64,
    pane: impl Fn(PaneId) -> AnyElement,
) -> AnyElement {
    render_node(layout, &format!("pane-layout-{revision}"), &pane, None)
}

/// Render a layout and notify the caller when a native divider is dragged.
pub fn render_layout_with_resize(
    layout: &LayoutNode,
    revision: u64,
    pane: impl Fn(PaneId) -> AnyElement,
    on_resize: ResizeCallback,
) -> AnyElement {
    render_node(
        layout,
        &format!("pane-layout-{revision}"),
        &pane,
        Some(on_resize),
    )
}

/// Use a path within the current layout revision as the kit group's stable key.
fn render_node(
    node: &LayoutNode,
    path: &str,
    pane: &impl Fn(PaneId) -> AnyElement,
    on_resize: Option<ResizeCallback>,
) -> AnyElement {
    match node {
        LayoutNode::Leaf(identity) => pane(*identity),
        LayoutNode::Split {
            direction,
            children,
        } => {
            let axis = match direction {
                SplitDirection::Horizontal => Axis::Horizontal,
                SplitDirection::Vertical => Axis::Vertical,
            };
            let mut group = ResizablePanelGroup::new(SharedString::from(path.to_owned()))
                .axis(axis)
                .children(children.iter().enumerate().map(|(index, child)| {
                    resizable_panel()
                        .size_range(px(0.0)..Pixels::MAX)
                        .flex_basis(px(0.0))
                        .flex_grow(f32::from(Pixels::from(child.weight)))
                        .child(render_node(
                            &child.node,
                            &format!("{path}-{index}"),
                            pane,
                            None,
                        ))
                }));
            if let Some(callback) = on_resize {
                group = group.on_resize(move |state, window, application| {
                    callback(state, window, application);
                });
            }
            group.into_any_element()
        }
    }
}
