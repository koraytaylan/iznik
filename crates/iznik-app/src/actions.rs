//! The closed action inventory shared by the palette and keymap.

use iznik_client::host::state::HostState;
use iznik_protocol::command::SessionCommand;

use crate::host_ui::EngineState;

/// Every user action that can be shown by the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionId {
    /// Create a session.
    CreateSession,
    /// Rename a session.
    RenameSession,
    /// Close a session.
    CloseSession,
    /// Reorder sessions.
    ReorderSessions,
    /// Create a tab.
    CreateTab,
    /// Rename a tab.
    RenameTab,
    /// Close a tab.
    CloseTab,
    /// Reorder tabs.
    ReorderTabs,
    /// Create a pane.
    CreatePane,
    /// Close a pane.
    ClosePane,
    /// Move a pane.
    MovePane,
    /// Set a tab layout.
    SetLayout,
    /// Add a host.
    AddHost,
    /// Remove a host.
    RemoveHost,
    /// Reconnect a host.
    ReconnectHost,
    /// Upgrade a host.
    UpgradeHost,
    /// Uninstall the server from a host.
    UninstallHost,
    /// Open the settings window.
    OpenSettings,
}

/// What an action operates on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTarget {
    /// A session command sent to the focused host.
    SessionCommand,
    /// A host manager operation.
    HostManager,
    /// A client-local effect with no protocol command.
    Application,
}

/// The context an action needs before it can be offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionContext {
    /// No selected model object is needed.
    None,
    /// A host is needed.
    Host,
    /// A session is needed.
    Session,
    /// A tab is needed.
    Tab,
    /// A pane is needed.
    Pane,
}

/// One inventory row shown by the command palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionSpec {
    /// The stable action identity.
    pub id: ActionId,
    /// The short name shown to a person.
    pub name: &'static str,
    /// The explanation shown below the name.
    pub explanation: &'static str,
    /// The operation which consumes the action.
    pub target: ActionTarget,
    /// The model context required before offering it.
    pub context: ActionContext,
    /// The default keybinding, when one exists.
    pub keybinding: Option<&'static str>,
}

/// The complete action table, in palette display order.
pub const INVENTORY: &[ActionSpec] = &[
    spec(
        ActionId::CreateSession,
        "session: new",
        "Create a session on the focused host.",
        ActionTarget::SessionCommand,
        ActionContext::Host,
        Some("ctrl-shift-n"),
    ),
    spec(
        ActionId::RenameSession,
        "session: rename",
        "Rename the selected session.",
        ActionTarget::SessionCommand,
        ActionContext::Session,
        None,
    ),
    spec(
        ActionId::CloseSession,
        "session: close",
        "Close the selected session and its tabs.",
        ActionTarget::SessionCommand,
        ActionContext::Session,
        None,
    ),
    spec(
        ActionId::ReorderSessions,
        "session: reorder",
        "Put the host's sessions in a new order.",
        ActionTarget::SessionCommand,
        ActionContext::Host,
        None,
    ),
    spec(
        ActionId::CreateTab,
        "tab: new",
        "Create a tab in the selected session.",
        ActionTarget::SessionCommand,
        ActionContext::Session,
        Some("ctrl-shift-t"),
    ),
    spec(
        ActionId::RenameTab,
        "tab: rename",
        "Rename the selected tab.",
        ActionTarget::SessionCommand,
        ActionContext::Tab,
        None,
    ),
    spec(
        ActionId::CloseTab,
        "tab: close",
        "Close the selected tab and its panes.",
        ActionTarget::SessionCommand,
        ActionContext::Tab,
        Some("ctrl-shift-w"),
    ),
    spec(
        ActionId::ReorderTabs,
        "tab: reorder",
        "Put the session's tabs in a new order.",
        ActionTarget::SessionCommand,
        ActionContext::Session,
        None,
    ),
    spec(
        ActionId::CreatePane,
        "pane: new",
        "Create a pane beside the selected pane.",
        ActionTarget::SessionCommand,
        ActionContext::Tab,
        Some("ctrl-shift-enter"),
    ),
    spec(
        ActionId::ClosePane,
        "pane: close",
        "Close the selected pane.",
        ActionTarget::SessionCommand,
        ActionContext::Pane,
        None,
    ),
    spec(
        ActionId::MovePane,
        "pane: move",
        "Move the selected pane into another tab.",
        ActionTarget::SessionCommand,
        ActionContext::Pane,
        None,
    ),
    spec(
        ActionId::SetLayout,
        "layout: set",
        "Replace a tab's pane arrangement.",
        ActionTarget::SessionCommand,
        ActionContext::Tab,
        None,
    ),
    spec(
        ActionId::AddHost,
        "host: add",
        "Begin holding and connecting a host.",
        ActionTarget::HostManager,
        ActionContext::None,
        Some("ctrl-shift-h"),
    ),
    spec(
        ActionId::RemoveHost,
        "host: remove",
        "Stop holding a host and forget its model.",
        ActionTarget::HostManager,
        ActionContext::Host,
        None,
    ),
    spec(
        ActionId::ReconnectHost,
        "host: reconnect",
        "Reconnect a failed or disconnected host.",
        ActionTarget::HostManager,
        ActionContext::Host,
        Some("ctrl-shift-r"),
    ),
    spec(
        ActionId::UpgradeHost,
        "host: upgrade",
        "Replace the host server with this build.",
        ActionTarget::HostManager,
        ActionContext::Host,
        None,
    ),
    spec(
        ActionId::UninstallHost,
        "host: uninstall",
        "Remove iznik-server from a host.",
        ActionTarget::HostManager,
        ActionContext::Host,
        None,
    ),
    spec(
        ActionId::OpenSettings,
        "iznik: settings",
        "Open the settings window.",
        ActionTarget::Application,
        ActionContext::None,
        None,
    ),
];

/// Build one inventory row without duplicating its field order.
const fn spec(
    id: ActionId,
    name: &'static str,
    explanation: &'static str,
    target: ActionTarget,
    context: ActionContext,
    keybinding: Option<&'static str>,
) -> ActionSpec {
    ActionSpec {
        id,
        name,
        explanation,
        target,
        context,
        keybinding,
    }
}

/// Whether an action has the model context it names.
#[must_use]
pub fn available(specification: &ActionSpec, state: &EngineState) -> bool {
    if specification.id == ActionId::ReconnectHost {
        return state.hosts().any(|(_, report)| {
            matches!(
                &report.connection,
                HostState::Disconnected | HostState::Reconnecting { .. } | HostState::Failed { .. }
            )
        });
    }
    if specification.id == ActionId::UpgradeHost {
        return state.hosts().any(|(_, report)| {
            matches!(
                &report.connection,
                HostState::Connected {
                    upgrade: Some(_),
                    ..
                }
            )
        });
    }
    if specification.id == ActionId::ReorderSessions {
        // Offered only where a connected server can answer it: sending the
        // command to a server that predates it ends the connection.
        return state
            .hosts()
            .any(|(host, _report)| state.reorders_sessions(host));
    }
    match specification.context {
        ActionContext::None => true,
        ActionContext::Host => state.hosts().next().is_some(),
        ActionContext::Session => state
            .model()
            .hosts
            .values()
            .any(|host| !host.model.sessions.is_empty()),
        ActionContext::Tab => state.model().hosts.values().any(|host| {
            host.model
                .sessions
                .iter()
                .any(|session| !session.tabs.is_empty())
        }),
        ActionContext::Pane => state.model().hosts.values().any(|host| {
            host.model
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .any(|tab| !tab.panes.is_empty())
        }),
    }
}

/// Whether a session command has a registered action.
#[must_use]
pub fn action_for_command(command: &SessionCommand) -> ActionId {
    match command {
        SessionCommand::CreateSession { .. } => ActionId::CreateSession,
        SessionCommand::RenameSession { .. } => ActionId::RenameSession,
        SessionCommand::CloseSession { .. } => ActionId::CloseSession,
        SessionCommand::ReorderSessions { .. } => ActionId::ReorderSessions,
        SessionCommand::CreateTab { .. } => ActionId::CreateTab,
        SessionCommand::RenameTab { .. } => ActionId::RenameTab,
        SessionCommand::CloseTab { .. } => ActionId::CloseTab,
        SessionCommand::ReorderTabs { .. } => ActionId::ReorderTabs,
        SessionCommand::CreatePane { .. } => ActionId::CreatePane,
        SessionCommand::ClosePane { .. } => ActionId::ClosePane,
        SessionCommand::MovePane { .. } => ActionId::MovePane,
        SessionCommand::SetLayout { .. } => ActionId::SetLayout,
    }
}
