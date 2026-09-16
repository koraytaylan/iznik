//! Model-driven session and tab bars for the application window.

use gpui_kit::component::Theme;
use gpui_kit::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    TestSupportExt, WeakEntity, div,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{SessionId, TabId};
use iznik_protocol::model::{Session, Tab};

use crate::host_ui::EngineState;
use crate::window::{TabKey, WindowShell};

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
#[must_use]
pub fn render(
    theme: &Theme,
    state: &EngineState,
    selected: Option<&TabKey>,
    shell: Option<&WeakEntity<WindowShell>>,
) -> Bars {
    let mut tabs = tab_bar_container(theme);
    let mut sessions = session_bar_container(theme);
    if state.model().hosts.is_empty() {
        tabs = tabs.child(
            div()
                .id("tab-bar-empty")
                .test_support()
                .text_color(theme.muted_foreground)
                .child("No hosts \u{2014} add a host to begin"),
        );
        sessions = sessions.child(
            div()
                .id("session-bar-empty")
                .test_support()
                .text_color(theme.muted_foreground)
                .child("No sessions"),
        );
    }
    for (host, view) in &state.model().hosts {
        let failed = state
            .host(host)
            .is_some_and(|report| matches!(report.connection, HostState::Failed { .. }));
        let status = state.host(host).map_or_else(
            || "unknown".to_owned(),
            |report| report.connection.to_string(),
        );
        if view.model.sessions.is_empty() {
            sessions = sessions.child(
                div()
                    .id(format!("session-bar-empty-{}", host.0))
                    .test_support()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} · no sessions", host.0)),
            );
        }
        if failed {
            sessions = sessions.child(
                div()
                    .id(format!("session-bar-failed-{}", host.0))
                    .test_support()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.danger)
                    .text_color(theme.danger_foreground)
                    .child(format!("{} · host failed", host.0)),
            );
        }
        for session in &view.model.sessions {
            let show_tabs = selected
                .is_some_and(|key| key.host == *host && key.session == session.id)
                || selected.is_none() && visible_session(state, host) == Some(session.id);
            sessions = sessions.child(session_entry(theme, shell, host, session, &status));
            if !show_tabs {
                continue;
            }
            let connected = state
                .host(host)
                .is_some_and(|report| matches!(report.connection, HostState::Connected { .. }));
            for tab in &session.tabs {
                let tab_key = TabKey {
                    host: host.clone(),
                    session: session.id,
                    tab: tab.id,
                };
                let is_selected = selected == Some(&tab_key);
                tabs = tabs.child(tab_entry(
                    theme,
                    shell,
                    host,
                    tab_key,
                    tab,
                    connected,
                    is_selected,
                ));
            }
        }
    }
    Bars {
        top: tabs.into_any_element(),
        bottom: sessions.into_any_element(),
    }
}

/// Build the empty top toolbar container, styled as a bar.
fn tab_bar_container(theme: &Theme) -> impl ParentElement + Styled + IntoElement {
    div()
        .id("tab-bar")
        .test_support()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .h_10()
        .px_2()
        .flex_shrink_0()
        .bg(theme.title_bar)
        .text_color(theme.foreground)
        .border_b_1()
        .border_color(theme.title_bar_border)
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

/// Render one session chip: name, host and connection status, with its close affordance.
fn session_entry(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    host: &HostId,
    session: &Session,
    status: &str,
) -> AnyElement {
    div()
        .id(format!("session-{}-{}", host.0, session.id.0))
        .test_support()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme.muted)
        .text_color(theme.muted_foreground)
        .child(format!(
            "{} \u{b7} {} \u{b7} {}",
            host.0, session.name, status
        ))
        .child(session_close(theme, shell, host, session.id))
        .into_any_element()
}

/// Render one tab chip: name, availability and selection state, with its close affordance.
fn tab_entry(
    theme: &Theme,
    shell: Option<&WeakEntity<WindowShell>>,
    host: &HostId,
    tab_key: TabKey,
    tab: &Tab,
    connected: bool,
    is_selected: bool,
) -> AnyElement {
    let marker = if connected { "" } else { " \u{b7} unavailable" };
    let (background, foreground) = if is_selected {
        (theme.tab_active, theme.tab_active_foreground)
    } else {
        (theme.tab, theme.tab_foreground)
    };
    let mut entry = div()
        .id(format!("tab-{}-{}", host.0, tab.id.0))
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
        .child(format!("{}{}", tab.name, marker))
        .child(tab_close(theme, shell, host, tab.id));
    if let Some(target) = shell.cloned() {
        entry = entry.on_click(move |_event, window, application| {
            let key = tab_key.clone();
            let _ignored = target.update(application, |window_shell, context| {
                let _selected = window_shell.select(key, window, context);
            });
        });
    }
    entry.into_any_element()
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
