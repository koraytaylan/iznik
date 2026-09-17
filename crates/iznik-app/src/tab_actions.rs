//! What can be done to one tab from the tab bar: its right-click menu, and
//! dragging it to another place among its session's tabs.
//!
//! Every order change is a whole `ReorderTabs` order computed here, so the
//! menu's moves and a drop agree on what a new order is.

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    App, Context, Hsla, IntoElement, ParentElement, Render, Styled, WeakEntity, Window, div,
};
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::TabId;

use crate::actions::ActionId;
use crate::window::{TabKey, WindowShell};

/// The order with `moving` placed where `onto` is: after it when moving
/// right, before it when moving left. `None` when nothing would change or
/// either tab is not in the order.
#[must_use]
pub fn dropped_order(order: &[TabId], moving: TabId, onto: TabId) -> Option<Vec<TabId>> {
    let from = order.iter().position(|tab| *tab == moving)?;
    let to = order.iter().position(|tab| *tab == onto)?;
    if from == to {
        return None;
    }
    let mut reordered: Vec<TabId> = order.iter().copied().filter(|tab| *tab != moving).collect();
    let target = reordered.iter().position(|tab| *tab == onto)?;
    let insert_at = if from < to {
        target.checked_add(1)?
    } else {
        target
    };
    reordered.insert(insert_at, moving);
    Some(reordered)
}

/// The order with `moving` one place left or right; `None` at that edge.
#[must_use]
pub fn shifted_order(order: &[TabId], moving: TabId, rightward: bool) -> Option<Vec<TabId>> {
    let from = order.iter().position(|tab| *tab == moving)?;
    let beside = if rightward {
        order.get(from.checked_add(1)?)
    } else {
        order.get(from.checked_sub(1)?)
    }?;
    dropped_order(order, moving, *beside)
}

/// The tabs "Close Other Tabs" closes.
#[must_use]
pub fn others(order: &[TabId], keep: TabId) -> Vec<TabId> {
    order.iter().copied().filter(|tab| *tab != keep).collect()
}

/// The tabs "Close Tabs to the Right" closes.
#[must_use]
pub fn to_the_right(order: &[TabId], of: TabId) -> Vec<TabId> {
    order
        .iter()
        .position(|tab| *tab == of)
        .and_then(|index| index.checked_add(1))
        .and_then(|start| order.get(start..))
        .map(<[TabId]>::to_vec)
        .unwrap_or_default()
}

/// A tab being dragged, carried to whatever it is dropped on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedTab {
    /// Which tab.
    pub key: TabKey,
    /// Its name, for the preview under the pointer.
    pub name: String,
}

/// The chip that follows the pointer while a tab is dragged.
#[derive(Debug)]
pub struct DragPreview {
    /// The tab's name.
    pub name: String,
    /// The chip's colour.
    pub background: Hsla,
    /// Its text colour.
    pub foreground: Hsla,
    /// Its border colour.
    pub border: Hsla,
}

impl Render for DragPreview {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(self.border)
            .bg(self.background)
            .text_color(self.foreground)
            .shadow_md()
            .child(self.name.clone())
    }
}

/// Reorder a session's tabs, reporting a refusal on the shell.
fn reorder(
    shell: &mut WindowShell,
    key: &TabKey,
    order: Vec<TabId>,
    context: &mut Context<'_, WindowShell>,
) {
    let command = SessionCommand::ReorderTabs {
        session: key.session,
        order,
    };
    if let Err(error) = shell.dispatch_command(&key.host.0, command) {
        shell.failure(&key.host, error.to_string(), context);
    }
}

/// Close tabs of one host, reporting a refusal on the shell.
fn close(
    shell: &mut WindowShell,
    key: &TabKey,
    tabs: &[TabId],
    context: &mut Context<'_, WindowShell>,
) {
    for tab in tabs {
        if let Err(error) =
            shell.dispatch_command(&key.host.0, SessionCommand::CloseTab { tab: *tab })
        {
            shell.failure(&key.host, error.to_string(), context);
            return;
        }
    }
}

/// Move a dropped tab to where it was dropped, when both are tabs of the same
/// session on the same host.
pub fn drop_onto(
    shell: &mut WindowShell,
    dragged: &DraggedTab,
    onto: &TabKey,
    order: &[TabId],
    context: &mut Context<'_, WindowShell>,
) {
    if dragged.key.host != onto.host || dragged.key.session != onto.session {
        return;
    }
    if let Some(reordered) = dropped_order(order, dragged.key.tab, onto.tab) {
        reorder(shell, onto, reordered, context);
    }
}

/// One menu item that runs `effect` on the shell.
fn item(
    label: &'static str,
    enabled: bool,
    shell: &WeakEntity<WindowShell>,
    effect: impl Fn(&mut WindowShell, &mut Window, &mut Context<'_, WindowShell>) + 'static,
) -> PopupMenuItem {
    let shell = shell.clone();
    PopupMenuItem::new(label).disabled(!enabled).on_click(
        move |_event, window, application: &mut App| {
            let _updated = shell.update(application, |shell, context| {
                effect(shell, window, context);
                context.notify();
            });
        },
    )
}

/// The right-click menu of one tab: new, rename, move, and close.
pub fn menu(
    shell: WeakEntity<WindowShell>,
    key: TabKey,
    order: Vec<TabId>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<'_, PopupMenu>) -> PopupMenu + 'static {
    move |menu, _menu_window, _menu_context| {
        let left = shifted_order(&order, key.tab, false);
        let right = shifted_order(&order, key.tab, true);
        let others = others(&order, key.tab);
        let rightward = to_the_right(&order, key.tab);
        let (new_key, rename_key, left_key, right_key, close_key, others_key, right_close_key) = (
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
            key.clone(),
        );
        menu.item(item(
            "New Tab",
            true,
            &shell,
            move |shell, window, context| {
                if shell.select(new_key.clone(), window, context) {
                    shell.palette_mut().open();
                    shell.choose(Some(ActionId::CreateTab), window, context);
                    shell.palette_mut().close();
                }
            },
        ))
        .item(item(
            "Rename Tab\u{2026}",
            true,
            &shell,
            move |shell, window, context| {
                if shell.select(rename_key.clone(), window, context) {
                    shell.palette_mut().open();
                    shell.choose(Some(ActionId::RenameTab), window, context);
                }
            },
        ))
        .separator()
        .item(item(
            "Move Left",
            left.is_some(),
            &shell,
            move |shell, _window, context| {
                if let Some(new_order) = left.clone() {
                    reorder(shell, &left_key, new_order, context);
                }
            },
        ))
        .item(item(
            "Move Right",
            right.is_some(),
            &shell,
            move |shell, _window, context| {
                if let Some(new_order) = right.clone() {
                    reorder(shell, &right_key, new_order, context);
                }
            },
        ))
        .separator()
        .item(item(
            "Close Tab",
            true,
            &shell,
            move |shell, _window, context| {
                close(shell, &close_key, &[close_key.tab], context);
            },
        ))
        .item(item(
            "Close Other Tabs",
            !others.is_empty(),
            &shell,
            move |shell, _window, context| {
                close(shell, &others_key, &others, context);
            },
        ))
        .item(item(
            "Close Tabs to the Right",
            !rightward.is_empty(),
            &shell,
            move |shell, _window, context| {
                close(shell, &right_close_key, &rightward, context);
            },
        ))
    }
}
