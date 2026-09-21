//! What can be done to one bar entry from its right-click menu: both a tab and
//! a session get a new entry, a rename, a move left or right and the three
//! close entries, and either can be dragged onto another entry of its own
//! strip — a tab among its session's tabs, a session among its host's
//! sessions.
//!
//! Every order change is a whole order computed here — a `ReorderTabs` order
//! for a tab, a `ReorderSessions` order for a session — so a menu's moves and
//! a drop agree on what a new order is.
//!
//! The menu is built and drawn here, and *opened* from the shell's own state
//! rather than through the kit's `ContextMenu` wrapper: that wrapper keeps its
//! open menu in element state it resets on every layout pass, so in a window
//! that repaints on a timer — this one polls its engine every sixteen
//! milliseconds — the menu vanishes on the next frame and its entity is never
//! released. The shell holds one open menu, renders it anchored where the
//! right click landed, and dismisses it on a press anywhere else or on Escape.

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    Anchor, AnyElement, App, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, Point, Render, Styled, Subscription, TestSupportExt as _, WeakEntity,
    Window, anchored, deferred, div, px,
};
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{SessionId, TabId};

use crate::actions::ActionId;
use crate::window::{SessionKey, TabKey, WindowShell};

/// The distance kept between an open tab menu and the window's edges.
const MENU_MARGIN: Pixels = px(8.);
/// The deferred paint priority of the open menu: above the bars, the grid and
/// the palette's own surface.
const MENU_LAYER: usize = 200;

/// The order with `moving` placed where `onto` is: after it when moving
/// right, before it when moving left. `None` when nothing would change or
/// either identity is not in the order. Shared by tabs and by sessions, whose
/// reorder rules are the same.
#[must_use]
pub fn dropped_order<Item: Copy + PartialEq>(
    order: &[Item],
    moving: Item,
    onto: Item,
) -> Option<Vec<Item>> {
    let from = order.iter().position(|held| *held == moving)?;
    let to = order.iter().position(|held| *held == onto)?;
    if from == to {
        return None;
    }
    let mut reordered: Vec<Item> = order
        .iter()
        .copied()
        .filter(|held| *held != moving)
        .collect();
    let target = reordered.iter().position(|held| *held == onto)?;
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
pub fn shifted_order<Item: Copy + PartialEq>(
    order: &[Item],
    moving: Item,
    rightward: bool,
) -> Option<Vec<Item>> {
    let from = order.iter().position(|held| *held == moving)?;
    let beside = if rightward {
        order.get(from.checked_add(1)?)
    } else {
        order.get(from.checked_sub(1)?)
    }?;
    dropped_order(order, moving, *beside)
}

/// The identities "Close Others" closes.
#[must_use]
pub fn others<Item: Copy + PartialEq>(order: &[Item], keep: Item) -> Vec<Item> {
    order.iter().copied().filter(|held| *held != keep).collect()
}

/// The identities "Close to the Right" closes.
#[must_use]
pub fn to_the_right<Item: Copy + PartialEq>(order: &[Item], of: Item) -> Vec<Item> {
    order
        .iter()
        .position(|held| *held == of)
        .and_then(|index| index.checked_add(1))
        .and_then(|start| order.get(start..))
        .map(<[Item]>::to_vec)
        .unwrap_or_default()
}

/// A bar entry being dragged, carried to whatever it is dropped on.
///
/// One payload covers both strips: a tab drags among its session's tabs, a
/// session drags among its host's sessions, and each is dropped only on an
/// entry of the same strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraggedEntry {
    /// A tab, dragged among its session's tabs.
    Tab {
        /// Which tab.
        key: TabKey,
        /// Its name, for the preview under the pointer.
        name: String,
    },
    /// A session, dragged among its host's sessions.
    Session {
        /// Which session.
        key: SessionKey,
        /// Its name, for the preview under the pointer.
        name: String,
    },
}

impl DraggedEntry {
    /// The name shown on the chip that follows the pointer.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            DraggedEntry::Tab { name, .. } | DraggedEntry::Session { name, .. } => name,
        }
    }

    /// The payload for dragging one tab.
    #[must_use]
    pub fn tab(key: TabKey, name: String) -> DraggedEntry {
        DraggedEntry::Tab { key, name }
    }

    /// The payload for dragging one session.
    #[must_use]
    pub fn session(key: SessionKey, name: String) -> DraggedEntry {
        DraggedEntry::Session { key, name }
    }
}

/// The chip that follows the pointer while a bar entry is dragged.
#[derive(Debug)]
pub struct DragPreview {
    /// The dragged entry's name.
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

/// Reorder the host's sessions, reporting a refusal on the shell.
fn reorder_sessions(
    shell: &mut WindowShell,
    key: &SessionKey,
    order: Vec<SessionId>,
    context: &mut Context<'_, WindowShell>,
) {
    let command = SessionCommand::ReorderSessions { order };
    if let Err(error) = shell.dispatch_command(&key.host.0, command) {
        shell.failure(&key.host, error.to_string(), context);
    }
}

/// Close sessions of one host, reporting a refusal on the shell.
fn close_sessions(
    shell: &mut WindowShell,
    key: &SessionKey,
    sessions: &[SessionId],
    context: &mut Context<'_, WindowShell>,
) {
    for session in sessions {
        let command = SessionCommand::CloseSession { session: *session };
        if let Err(error) = shell.dispatch_command(&key.host.0, command) {
            shell.failure(&key.host, error.to_string(), context);
            return;
        }
    }
}

/// Move a dropped tab to where it was dropped, when both are tabs of the same
/// session on the same host. A dropped session is ignored here; it is handled
/// by [`drop_session_onto`].
pub fn drop_onto(
    shell: &mut WindowShell,
    dragged: &DraggedEntry,
    onto: &TabKey,
    order: &[TabId],
    context: &mut Context<'_, WindowShell>,
) {
    let DraggedEntry::Tab { key, .. } = dragged else {
        return;
    };
    if key.host != onto.host || key.session != onto.session {
        return;
    }
    if let Some(reordered) = dropped_order(order, key.tab, onto.tab) {
        reorder(shell, onto, reordered, context);
    }
}

/// Move a dropped session to where it was dropped, when both are sessions of
/// the same host and that host's server advertised it can reorder sessions. A
/// dropped tab is ignored here; it is handled by [`drop_onto`]. A server that
/// cannot decode `ReorderSessions` ends the connection on it, so a drop on
/// such a host is ignored rather than sent.
pub fn drop_session_onto(
    shell: &mut WindowShell,
    dragged: &DraggedEntry,
    onto: &SessionKey,
    order: &[SessionId],
    context: &mut Context<'_, WindowShell>,
) {
    let DraggedEntry::Session { key, .. } = dragged else {
        return;
    };
    if key.host != onto.host || !shell.hosts().state().reorders_sessions(&onto.host) {
        return;
    }
    if let Some(reordered) = dropped_order(order, key.session, onto.session) {
        reorder_sessions(shell, onto, reordered, context);
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

/// The bar entry a right-click menu is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A tab of one session.
    Tab,
    /// A whole session.
    Session,
}

impl Subject {
    /// One word for the entry the menu belongs to, used in the element id a
    /// case finds it by.
    fn word(self) -> &'static str {
        match self {
            Subject::Tab => "tab",
            Subject::Session => "session",
        }
    }
}

/// One open bar menu: which entry it is about, where it was opened, and the
/// menu entity drawn, held so its entity lives exactly as long as this is
/// open.
#[derive(Debug)]
pub struct OpenMenu {
    /// The entry it is about, for the element id a case finds it by.
    subject: Subject,
    /// Where the right click landed, in window coordinates.
    pub position: Point<Pixels>,
    /// The built menu.
    pub menu: Entity<PopupMenu>,
    /// Ends when the menu dismisses, which is what closes this.
    _ended: Subscription,
}

impl OpenMenu {
    /// Hold a menu entity opened at `position`, closing it through the shell
    /// when it dismisses itself.
    pub fn opened(
        subject: Subject,
        position: Point<Pixels>,
        menu: Entity<PopupMenu>,
        window: &mut Window,
        context: &mut Context<'_, WindowShell>,
    ) -> OpenMenu {
        let ended = context.subscribe_in(
            &menu,
            window,
            |shell: &mut WindowShell, _menu, _event: &gpui_kit::DismissEvent, _window, context| {
                shell.close_menu(context);
            },
        );
        OpenMenu {
            subject,
            position,
            menu,
            _ended: ended,
        }
    }
}

impl WindowShell {
    /// Open the right-click menu of one tab where the click landed, replacing
    /// whatever menu was open.
    pub fn open_tab_menu(
        &mut self,
        key: TabKey,
        order: Vec<TabId>,
        position: Point<Pixels>,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        let entity = context.entity().downgrade();
        let menu = PopupMenu::build(window, context, tab_menu(entity, key, order));
        self.menu = Some(OpenMenu::opened(
            Subject::Tab,
            position,
            menu,
            window,
            context,
        ));
        context.notify();
    }

    /// Open the right-click menu of one session where the click landed,
    /// replacing whatever menu was open.
    pub fn open_session_menu(
        &mut self,
        key: SessionKey,
        order: Vec<SessionId>,
        position: Point<Pixels>,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        let entity = context.entity().downgrade();
        let reorderable = self.hosts().state().reorders_sessions(&key.host);
        let menu = PopupMenu::build(
            window,
            context,
            session_menu(entity, key, order, reorderable),
        );
        self.menu = Some(OpenMenu::opened(
            Subject::Session,
            position,
            menu,
            window,
            context,
        ));
        context.notify();
    }

    /// Close the open menu, if one is open.
    pub fn close_menu(&mut self, context: &mut Context<'_, Self>) {
        if self.menu.take().is_some() {
            context.notify();
        }
    }

    /// Rename the session holding a right-clicked entry by opening the
    /// palette at the rename prompt for that session.
    fn prompt_session_name(
        &mut self,
        key: &SessionKey,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        self.select_session(key, window, context);
        self.palette_mut().open();
        self.choose(Some(ActionId::RenameSession), window, context);
    }
}

/// Render the open menu over the window, anchored where it was opened.
#[must_use]
pub fn render_open(open: &OpenMenu) -> AnyElement {
    deferred(
        anchored()
            .position(open.position)
            .anchor(Anchor::TopLeft)
            .snap_to_window_with_margin(MENU_MARGIN)
            .child(
                div()
                    .id(format!("{}-menu", open.subject.word()))
                    .test_support()
                    .occlude()
                    .child(open.menu.clone()),
            ),
    )
    .with_priority(MENU_LAYER)
    .into_any_element()
}

/// The right-click menu of one session: new, rename, move, and close, the
/// same entries a tab's menu offers.
///
/// `reorderable` is whether the connected server advertised it can reorder
/// sessions. A server of a build that predates `ReorderSessions` refuses the
/// command as garbage and ends the connection on it, so the moves are offered
/// enabled only to a server that can answer them — and disabled, rather than
/// hidden, so the entry and its reason stay visible.
pub fn session_menu(
    shell: WeakEntity<WindowShell>,
    key: SessionKey,
    order: Vec<SessionId>,
    reorderable: bool,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<'_, PopupMenu>) -> PopupMenu + 'static {
    move |menu, _menu_window, _menu_context| {
        let left = shifted_order(&order, key.session, false);
        let right = shifted_order(&order, key.session, true);
        let others = others(&order, key.session);
        let rightward = to_the_right(&order, key.session);
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
            "New Session",
            true,
            &shell,
            move |shell, window, context| {
                shell.select_session(&new_key, window, context);
                if let Err(error) = shell.dispatch_action_on(ActionId::CreateSession, &new_key.host)
                {
                    shell.failure(&new_key.host, error.to_string(), context);
                }
            },
        ))
        .item(item(
            "Rename Session\u{2026}",
            true,
            &shell,
            move |shell, window, context| {
                shell.prompt_session_name(&rename_key, window, context);
            },
        ))
        .separator()
        .item(item(
            if reorderable {
                "Move Left"
            } else {
                "Move Left (needs a newer server)"
            },
            reorderable && left.is_some(),
            &shell,
            move |shell, _window, context| {
                if let Some(new_order) = left.clone() {
                    reorder_sessions(shell, &left_key, new_order, context);
                }
            },
        ))
        .item(item(
            if reorderable {
                "Move Right"
            } else {
                "Move Right (needs a newer server)"
            },
            reorderable && right.is_some(),
            &shell,
            move |shell, _window, context| {
                if let Some(new_order) = right.clone() {
                    reorder_sessions(shell, &right_key, new_order, context);
                }
            },
        ))
        .separator()
        .item(item(
            "Close Session",
            true,
            &shell,
            move |shell, _window, context| {
                close_sessions(shell, &close_key, &[close_key.session], context);
            },
        ))
        .item(item(
            "Close Other Sessions",
            !others.is_empty(),
            &shell,
            move |shell, _window, context| {
                close_sessions(shell, &others_key, &others, context);
            },
        ))
        .item(item(
            "Close Sessions to the Right",
            !rightward.is_empty(),
            &shell,
            move |shell, _window, context| {
                close_sessions(shell, &right_close_key, &rightward, context);
            },
        ))
    }
}

/// The right-click menu of one tab: new, rename, move, and close.
pub fn tab_menu(
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
