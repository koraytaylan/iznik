//! Model-driven session and tab bars for the application window.

use gpui_kit::component::Theme;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::{
    AnyElement, AppContext as _, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt, WeakEntity, div,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{SessionId, TabId};
use iznik_protocol::model::{Session, Tab};

use crate::actions::ActionId;
use crate::host_ui::EngineState;
use crate::status;
use crate::tab_actions::{self, DragPreview, DraggedTab};
use crate::window::{TabKey, WindowShell};

/// Where the tab strip is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabPlacement {
    /// A bar of its own, under the window's title bar.
    Bar,
    /// Inside the window's title bar, which draws the background.
    TitleBar,
}

/// The two toolbars the window places at its top and bottom edges.
pub struct Bars {
    /// The tab strip for the focused session, placed above the pane area.
    pub top: AnyElement,
    /// The session strip for every host, placed below the pane area.
    pub bottom: AnyElement,
}

impl std::fmt::Debug for Bars {
    /// Elements carry no debug representation of their own; name the type only.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Bars").finish_non_exhaustive()
    }
}

/// Render the top tab strip and bottom session strip from the settled client model.
///
/// The tab strip holds the selected session's tabs and a button for another.
/// The session strip groups every host's sessions behind the host's name and a
/// one-word state, with a button for another session on each connected host;
/// with no host held it says so.
#[must_use]
pub fn render(
    theme: &Theme,
    state: &EngineState,
    selected: Option<&TabKey>,
    shell: Option<&WeakEntity<WindowShell>>,
) -> Bars {
    render_placed(theme, state, selected, shell, TabPlacement::Bar)
}

/// The same, with the tab strip drawn for `placement`.
#[must_use]
pub fn render_placed(
    theme: &Theme,
    state: &EngineState,
    selected: Option<&TabKey>,
    shell: Option<&WeakEntity<WindowShell>>,
    placement: TabPlacement,
) -> Bars {
    let mut tabs = tab_bar_container(theme, placement);
    let mut sessions = session_bar_container(theme);
    if state.hosts().next().is_none() && state.model().hosts.is_empty() {
        sessions = sessions.child(
            div()
                .id("session-bar-empty")
                .test_support()
                .text_color(theme.muted_foreground)
                .child("No hosts"),
        );
    }
    let aliases: std::collections::BTreeSet<&HostId> = state
        .hosts()
        .map(|(host, _)| host)
        .chain(state.model().hosts.keys())
        .collect();
    for host in aliases {
        let connection = state.host(host).map(|report| &report.connection);
        let connected = matches!(connection, Some(HostState::Connected { .. }));
        sessions = sessions.child(host_label(theme, host, connection));
        let Some(view) = state.model().host(host) else {
            continue;
        };
        for session in &view.model.sessions {
            let is_selected =
                selected.is_some_and(|key| key.host == *host && key.session == session.id);
            let show_tabs = is_selected
                || selected.is_none() && visible_session(state, host) == Some(session.id);
            sessions = sessions.child(session_entry(
                theme,
                shell,
                state,
                host,
                session,
                is_selected,
            ));
            if !show_tabs {
                continue;
            }
            let order: Vec<TabId> = session.tabs.iter().map(|tab| tab.id).collect();
            for tab in &session.tabs {
                let tab_key = TabKey {
                    host: host.clone(),
                    session: session.id,
                    tab: tab.id,
                };
                let tab_selected = selected == Some(&tab_key);
                tabs = tabs.child(tab_entry(
                    theme,
                    shell,
                    TabChip {
                        key: tab_key,
                        tab,
                        order: &order,
                        connected,
                        selected: tab_selected,
                    },
                ));
            }
            if connected {
                tabs = tabs.child(add_button(
                    theme,
                    shell,
                    "tab-new".to_owned(),
                    "New tab",
                    None,
                ));
            }
        }
        if connected {
            sessions = sessions.child(add_button(
                theme,
                shell,
                format!("session-new-{}", host.0),
                "New session",
                Some(host.clone()),
            ));
        }
    }
    Bars {
        top: tabs.into_any_element(),
        bottom: sessions.into_any_element(),
    }
}

/// A host's name in the session strip, led by a dot in its state's tone and
/// followed by the state in one word while it is not connected.
fn host_label(theme: &Theme, host: &HostId, connection: Option<&HostState>) -> AnyElement {
    let (tone, word) = connection.map_or((status::Tone::Quiet, Some("unknown")), |state| {
        let summary = status::summary(host, state);
        let word = (!matches!(state, HostState::Connected { .. })).then(|| status::word(state));
        (summary.tone, word)
    });
    let text = word.map_or_else(
        || host.0.clone(),
        |word| format!("{} \u{b7} {word}", host.0),
    );
    div()
        .id(format!("host-label-{}", host.0))
        .test_support()
        .flex()
        .items_center()
        .gap_1()
        .pl_1()
        .text_color(theme.muted_foreground)
        .child(status::dot(theme, tone))
        .child(text)
        .into_any_element()
}

/// A small "+" chip that creates a tab in the selected session, or a session
/// on the named host.
fn add_button(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    identifier: String,
    label: &'static str,
    host: Option<HostId>,
) -> AnyElement {
    let mut button = div()
        .id(identifier)
        .test_support()
        .aria_label(label)
        .px_2()
        .rounded_md()
        .text_color(theme.muted_foreground)
        .hover(|style| style.bg(theme.muted).text_color(theme.foreground))
        .child("+");
    if let Some(target) = shell.cloned() {
        button = button.on_click(move |_event, _window, application| {
            let host = host.clone();
            let _ignored = target.update(application, |window_shell, context| {
                let result = match &host {
                    Some(host) => window_shell.dispatch_action_on(ActionId::CreateSession, host),
                    None => window_shell.dispatch_action(ActionId::CreateTab),
                };
                if let Err(error) = result {
                    let about = host.unwrap_or_else(|| HostId("tab bar".to_owned()));
                    window_shell.failure(&about, error.to_string(), context);
                }
            });
        });
    }
    button.into_any_element()
}

/// Build the empty top tab strip: a bar of its own, or a transparent strip
/// that fills the title bar it sits in.
fn tab_bar_container(
    theme: &Theme,
    placement: TabPlacement,
) -> impl ParentElement + Styled + IntoElement {
    let strip = div()
        .id("tab-bar")
        .test_support()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .overflow_x_hidden()
        .text_color(theme.foreground);
    match placement {
        TabPlacement::Bar => strip
            .w_full()
            .h_10()
            .px_2()
            .flex_shrink_0()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border),
        TabPlacement::TitleBar => strip.h_full().flex_1(),
    }
}

/// Build the empty bottom toolbar container, styled as a bar.
fn session_bar_container(theme: &Theme) -> impl ParentElement + Styled + IntoElement {
    div()
        .id("session-bar")
        .test_support()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .h_8()
        .px_2()
        .flex_shrink_0()
        .bg(theme.status_bar)
        .text_color(theme.foreground)
        .border_t_1()
        .border_color(theme.status_bar_border)
}

/// Render one session chip: its name, highlighted while selected, choosing
/// it on a click, with its close affordance.
fn session_entry(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    state: &EngineState,
    host: &HostId,
    session: &Session,
    is_selected: bool,
) -> AnyElement {
    let (background, foreground) = if is_selected {
        (theme.tab_active, theme.tab_active_foreground)
    } else {
        (theme.muted, theme.muted_foreground)
    };
    let mut entry = div()
        .id(format!("session-{}-{}", host.0, session.id.0))
        .test_support()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(background)
        .text_color(foreground)
        .hover(|style| style.bg(theme.tab_active))
        .child(session.name.clone())
        .child(session_close(theme, shell, host, session.id));
    let target_tab = focused_tab(state, host, session).map(|tab| TabKey {
        host: host.clone(),
        session: session.id,
        tab,
    });
    if let (Some(target), Some(key)) = (shell.cloned(), target_tab) {
        entry = entry.on_click(move |_event, window, application| {
            let key = key.clone();
            let _ignored = target.update(application, |window_shell, context| {
                let _selected = window_shell.select(key, window, context);
            });
        });
    }
    entry.into_any_element()
}

/// The tab of a session holding the host's focused pane, or its first tab.
fn focused_tab(state: &EngineState, host: &HostId, session: &Session) -> Option<TabId> {
    let focus = state.model().host(host).and_then(|view| view.focus);
    session
        .tabs
        .iter()
        .find(|tab| tab.panes.iter().any(|pane| Some(pane.id) == focus))
        .or_else(|| session.tabs.first())
        .map(|tab| tab.id)
}

/// What one tab chip shows and acts on.
struct TabChip<'model> {
    /// The tab's host-qualified identity.
    key: TabKey,
    /// The tab itself.
    tab: &'model Tab,
    /// Its session's tabs, in order, for moves and drops.
    order: &'model [TabId],
    /// Whether its host is connected.
    connected: bool,
    /// Whether it is the selected tab.
    selected: bool,
}

/// Render one tab chip: name, availability and selection state, with its
/// close affordance; a click selects it, a right click opens its menu, and it
/// can be dragged onto another tab of its session to move it there.
fn tab_entry(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    chip: TabChip<'_>,
) -> AnyElement {
    let TabChip {
        key,
        tab,
        order,
        connected,
        selected,
    } = chip;
    let marker = if connected { "" } else { " \u{b7} unavailable" };
    let (background, foreground) = if selected {
        (theme.tab_active, theme.tab_active_foreground)
    } else {
        (theme.tab, theme.tab_foreground)
    };
    let mut entry = div()
        .id(format!("tab-{}-{}", key.host.0, tab.id.0))
        .test_support()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(background)
        .bg(background)
        .text_color(foreground)
        .hover(|style| style.bg(theme.tab_active))
        // A press here is the tab's, not the title bar's: without this, a tab
        // in the title bar would start moving the window instead of itself.
        .on_mouse_down(MouseButton::Left, |_event, _window, application| {
            application.stop_propagation();
        })
        .child(format!("{}{}", tab.name, marker))
        .child(tab_close(theme, shell, &key.host, tab.id));
    let Some(target) = shell.cloned() else {
        return entry.into_any_element();
    };
    let dragged = DraggedTab {
        key: key.clone(),
        name: tab.name.clone(),
    };
    let (preview_background, preview_foreground, preview_border) =
        (theme.tab_active, theme.tab_active_foreground, theme.border);
    let drop_target = target.clone();
    let drop_key = key.clone();
    let drop_order = order.to_vec();
    let accent = theme.accent;
    let click_key = key.clone();
    let click_target = target.clone();
    entry = entry
        .on_click(move |_event, window, application| {
            let clicked = click_key.clone();
            let _ignored = click_target.update(application, |window_shell, context| {
                let _selected = window_shell.select(clicked, window, context);
            });
        })
        .on_drag(dragged, move |dragged, _offset, _window, application| {
            application.new(|_context| DragPreview {
                name: dragged.name.clone(),
                background: preview_background,
                foreground: preview_foreground,
                border: preview_border,
            })
        })
        .drag_over::<DraggedTab>(move |style, _dragged, _window, _application| {
            style.border_color(accent)
        })
        .on_drop(move |dragged: &DraggedTab, _window, application| {
            let _ignored = drop_target.update(application, |window_shell, context| {
                tab_actions::drop_onto(window_shell, dragged, &drop_key, &drop_order, context);
            });
        });
    entry
        .context_menu(tab_actions::menu(target, key, order.to_vec()))
        .into_any_element()
}

/// Render the tab close affordance and route it through the shell bridge.
fn tab_close(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    host: &HostId,
    tab: TabId,
) -> AnyElement {
    let mut close = div()
        .id(format!("tab-close-{}-{}", host.0, tab.0))
        .test_support()
        .px_1()
        .rounded_sm()
        .hover(|style| style.bg(theme.danger).text_color(theme.danger_foreground))
        .child("\u{d7}");
    if let Some(target) = shell.cloned() {
        let alias = host.0.clone();
        close = close.on_click(move |_event, _window, application| {
            let command = SessionCommand::CloseTab { tab };
            let _ignored = target.update(application, |window_shell, _context| {
                let _submission = window_shell.dispatch_command(&alias, command);
            });
        });
    }
    close.into_any_element()
}

/// Render the session close affordance and route it through the shell bridge.
fn session_close(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    host: &HostId,
    session: SessionId,
) -> AnyElement {
    let mut close = div()
        .id(format!("session-close-{}-{}", host.0, session.0))
        .test_support()
        .px_1()
        .rounded_sm()
        .hover(|style| style.bg(theme.danger).text_color(theme.danger_foreground))
        .child("\u{d7}");
    if let Some(target) = shell.cloned() {
        let alias = host.0.clone();
        close = close.on_click(move |_event, _window, application| {
            let command = SessionCommand::CloseSession { session };
            let _ignored = target.update(application, |window_shell, _context| {
                let _submission = window_shell.dispatch_command(&alias, command);
            });
        });
    }
    close.into_any_element()
}

/// Return the session containing a host's focused pane, when the model has one.
#[must_use]
pub fn focused_session(state: &EngineState, host: &HostId) -> Option<SessionId> {
    let view = state.model().host(host)?;
    let pane = view.focus?;
    view.model
        .sessions
        .iter()
        .find(|session| {
            session
                .tabs
                .iter()
                .any(|tab| tab.panes.iter().any(|item| item.id == pane))
        })
        .map(|session| session.id)
}

/// Return the focused session, or the first model session before focus exists.
#[must_use]
fn visible_session(state: &EngineState, host: &HostId) -> Option<SessionId> {
    focused_session(state, host).or_else(|| {
        state
            .model()
            .host(host)
            .and_then(|view| view.model.sessions.first().map(|session| session.id))
    })
}

/// Return the ordered tabs in the selected or model-focused session.
#[must_use]
pub fn tab_keys(state: &EngineState, selected: Option<&TabKey>) -> Vec<TabKey> {
    let chosen = selected
        .map(|key| (key.host.clone(), key.session))
        .or_else(|| {
            state.model().hosts.iter().find_map(|(host, view)| {
                let session = view.focus.and_then(|pane| {
                    view.model.sessions.iter().find(|candidate| {
                        candidate
                            .tabs
                            .iter()
                            .any(|tab| tab.panes.iter().any(|item| item.id == pane))
                    })
                });
                let session = session.or_else(|| view.model.sessions.first())?;
                Some((host.clone(), session.id))
            })
        });
    let Some((host, session_id)) = chosen else {
        return Vec::new();
    };
    state
        .model()
        .host(&host)
        .into_iter()
        .flat_map(|view| &view.model.sessions)
        .find(|session| session.id == session_id)
        .into_iter()
        .flat_map(|session| session.tabs.iter())
        .map(|tab| TabKey {
            host: host.clone(),
            session: session_id,
            tab: tab.id,
        })
        .collect()
}

/// Return the next tab in model order, wrapping at either end.
#[must_use]
pub fn next_tab(state: &EngineState, selected: Option<&TabKey>, forward: bool) -> Option<TabKey> {
    let keys = tab_keys(state, selected);
    let current = selected.and_then(|key| keys.iter().position(|candidate| candidate == key));
    let index = current.unwrap_or(if forward {
        0
    } else {
        keys.len().saturating_sub(1)
    });
    keys.get(next_index(index, keys.len(), forward)).cloned()
}

/// Move a bar selection by one entry, wrapping at either end.
#[must_use]
pub fn next_index(current: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        0
    } else if forward {
        if current >= count.saturating_sub(1) {
            0
        } else {
            current.saturating_add(1)
        }
    } else if current == 0 {
        count.saturating_sub(1)
    } else {
        current.saturating_sub(1)
    }
}

/// Build the protocol command emitted when a tab close affordance is chosen.
#[must_use]
pub fn close_tab(tab: TabId) -> SessionCommand {
    SessionCommand::CloseTab { tab }
}

/// Build the protocol command emitted when a session close affordance is chosen.
#[must_use]
pub fn close_session(session: SessionId) -> SessionCommand {
    SessionCommand::CloseSession { session }
}
