//! The argument step for palette actions that need a name, a destination or
//! an arrangement before they can be sent.
//!
//! Choosing such an action turns the palette's query into the answer: a typed
//! name or host alias, or a fuzzy filter over a fixed set of complete commands
//! built from the model at the moment the action was chosen.

use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;
use iznik_protocol::command::{Placement, SessionCommand};
use iznik_protocol::identity::{PaneId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Session, SplitDirection, Tab, Weighted};

use crate::actions::ActionId;
use crate::host_ui::EngineState;
use crate::palette::fuzzy_match;
use crate::window::TabKey;

/// The share every pane gets when an arrangement lays them out evenly.
const EVEN_WEIGHT: u32 = 1;

/// A palette action waiting for its argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// The action the argument completes.
    pub action: ActionId,
    /// The question shown above the answer.
    pub question: String,
    /// The text the answer starts from, such as the name being replaced.
    pub initial: String,
    /// How the answer becomes an operation.
    pub expected: Expected,
}

/// The shape of the argument a prompt waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expected {
    /// A host alias to begin holding.
    Alias,
    /// A new name for a session.
    SessionName {
        /// The host that owns the session.
        host: HostId,
        /// The session to rename.
        session: iznik_protocol::identity::SessionId,
    },
    /// A new name for a tab.
    TabName {
        /// The host that owns the tab.
        host: HostId,
        /// The tab to rename.
        tab: TabId,
    },
    /// One of a fixed set of complete operations.
    Choice(Vec<Choice>),
}

/// One complete operation offered by a choice prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// What a person reads to pick it.
    pub label: String,
    /// The operation performed when it is picked.
    pub answer: Answer,
}

/// A host manager operation that acts on one held host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOperation {
    /// Stop holding it.
    Remove,
    /// Reconnect it now.
    Reconnect,
    /// Replace its server with this build's.
    Upgrade,
    /// Take iznik off it.
    Uninstall,
}

/// The operation an answered prompt asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Begin holding and connecting the host with this alias.
    AddHost(String),
    /// Perform a host manager operation on one host.
    Host {
        /// Which operation.
        operation: HostOperation,
        /// The host it acts on.
        host: HostId,
    },
    /// Send this session command to this host.
    Command {
        /// The host the command is for.
        host: HostId,
        /// The complete command.
        command: SessionCommand,
    },
}

/// What choosing an action leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Ask for the argument first.
    Ask(Prompt),
    /// Nothing is left to ask: perform this.
    Perform(Answer),
}

/// What an action needs before it can be performed, or `None` when it is an
/// argument-free session command or the model holds nothing for it to act on.
#[must_use]
pub fn begin(action: ActionId, state: &EngineState, selected: Option<&TabKey>) -> Option<Step> {
    if action == ActionId::AddHost {
        return Some(Step::Ask(Prompt {
            action,
            question: "Host to add: an ssh alias, or unix:/path/to/socket".to_owned(),
            initial: String::new(),
            expected: Expected::Alias,
        }));
    }
    if let Some(operation) = host_operation(action) {
        return host_step(action, operation, state, selected);
    }
    ask(action, state, selected).map(Step::Ask)
}

/// The host manager operation an action performs, when it performs one.
fn host_operation(action: ActionId) -> Option<HostOperation> {
    match action {
        ActionId::RemoveHost => Some(HostOperation::Remove),
        ActionId::ReconnectHost => Some(HostOperation::Reconnect),
        ActionId::UpgradeHost => Some(HostOperation::Upgrade),
        ActionId::UninstallHost => Some(HostOperation::Uninstall),
        _ => None,
    }
}

/// Whether a host operation makes sense for a host in this state.
fn applies(operation: HostOperation, connection: &HostState) -> bool {
    match operation {
        HostOperation::Remove | HostOperation::Uninstall => true,
        HostOperation::Reconnect => !matches!(connection, HostState::Connected { .. }),
        HostOperation::Upgrade => matches!(
            connection,
            HostState::Connected {
                upgrade: Some(_),
                ..
            }
        ),
    }
}

/// A host operation performed at once on the only host it applies to, or a
/// choice between the hosts when several apply. Uninstalling always asks, so
/// the ending of every session on the host is confirmed rather than assumed.
fn host_step(
    action: ActionId,
    operation: HostOperation,
    state: &EngineState,
    selected: Option<&TabKey>,
) -> Option<Step> {
    let mut hosts: Vec<&HostId> = state
        .hosts()
        .filter(|(_, report)| applies(operation, &report.connection))
        .map(|(host, _)| host)
        .collect();
    // The selected tab's host is the one a person most likely means.
    if let Some(position) =
        selected.and_then(|key| hosts.iter().position(|host| **host == key.host))
    {
        let chosen = hosts.remove(position);
        hosts.insert(0, chosen);
    }
    if operation != HostOperation::Uninstall
        && let [only] = hosts.as_slice()
    {
        return Some(Step::Perform(Answer::Host {
            operation,
            host: (*only).clone(),
        }));
    }
    if hosts.is_empty() {
        return None;
    }
    let (question, verb) = match operation {
        HostOperation::Remove => ("Stop holding which host?", "Remove"),
        HostOperation::Reconnect => ("Reconnect which host?", "Reconnect"),
        HostOperation::Upgrade => ("Upgrade the server on which host?", "Upgrade"),
        HostOperation::Uninstall => (
            "Uninstall iznik-server? This ends every session on the host.",
            "Uninstall from",
        ),
    };
    Some(Step::Ask(Prompt {
        action,
        question: question.to_owned(),
        initial: String::new(),
        expected: Expected::Choice(
            hosts
                .into_iter()
                .map(|host| Choice {
                    label: format!("{verb} {}", host.0),
                    answer: Answer::Host {
                        operation,
                        host: host.clone(),
                    },
                })
                .collect(),
        ),
    }))
}

/// The prompt a session action needs over the selected tab.
fn ask(action: ActionId, state: &EngineState, selected: Option<&TabKey>) -> Option<Prompt> {
    let key = selected?;
    let model = &state.model().host(&key.host)?.model;
    let session = model
        .sessions
        .iter()
        .find(|session| session.id == key.session)?;
    let tab = session.tabs.iter().find(|tab| tab.id == key.tab)?;
    let host = key.host.clone();
    let (question, initial, expected) = match action {
        ActionId::RenameSession => (
            format!("New name for session \u{201C}{}\u{201D}", session.name),
            session.name.clone(),
            Expected::SessionName {
                host,
                session: session.id,
            },
        ),
        ActionId::RenameTab => (
            format!("New name for tab \u{201C}{}\u{201D}", tab.name),
            tab.name.clone(),
            Expected::TabName { host, tab: tab.id },
        ),
        ActionId::ReorderTabs => choice(
            format!("Move tab \u{201C}{}\u{201D}", tab.name),
            &host,
            reorder_choices(session, tab.id),
        ),
        ActionId::MovePane => {
            let pane = moving_pane(state, key, tab)?;
            choice(
                "Move the pane to tab".to_owned(),
                &host,
                move_choices(model, tab.id, pane),
            )
        }
        ActionId::SetLayout => choice(
            format!("Arrange tab \u{201C}{}\u{201D}", tab.name),
            &host,
            layout_choices(tab),
        ),
        _ => return None,
    };
    Some(Prompt {
        action,
        question,
        initial,
        expected,
    })
}

/// A choice prompt's question, empty starting answer and choices, each
/// labelled command sent to one host.
fn choice(
    question: String,
    host: &HostId,
    commands: Vec<(String, SessionCommand)>,
) -> (String, String, Expected) {
    let choices = commands
        .into_iter()
        .map(|(label, command)| Choice {
            label,
            answer: Answer::Command {
                host: host.clone(),
                command,
            },
        })
        .collect();
    (question, String::new(), Expected::Choice(choices))
}

/// The choices of a prompt that fuzzy-match the answer typed so far.
#[must_use]
pub fn choices<'prompt>(prompt: &'prompt Prompt, answer: &str) -> Vec<&'prompt Choice> {
    let Expected::Choice(choices) = &prompt.expected else {
        return Vec::new();
    };
    let normalized = answer.to_lowercase();
    choices
        .iter()
        .filter(|choice| fuzzy_match(&choice.label.to_lowercase(), &normalized))
        .collect()
}

/// The operation a prompt's answer asks for, or `None` while the answer is
/// an empty name or matches no choice.
#[must_use]
pub fn answer(prompt: &Prompt, answer: &str, selected: usize) -> Option<Answer> {
    let name = answer.trim();
    match &prompt.expected {
        Expected::Choice(_) => choices(prompt, answer)
            .get(selected)
            .map(|choice| choice.answer.clone()),
        _ if name.is_empty() => None,
        Expected::Alias => Some(Answer::AddHost(name.to_owned())),
        Expected::SessionName { host, session } => Some(Answer::Command {
            host: host.clone(),
            command: SessionCommand::RenameSession {
                session: *session,
                name: name.to_owned(),
            },
        }),
        Expected::TabName { host, tab } => Some(Answer::Command {
            host: host.clone(),
            command: SessionCommand::RenameTab {
                tab: *tab,
                name: name.to_owned(),
            },
        }),
    }
}

/// Every other position the tab can take in its session, as whole orders.
fn reorder_choices(session: &Session, moving: TabId) -> Vec<(String, SessionCommand)> {
    let others: Vec<&Tab> = session.tabs.iter().filter(|tab| tab.id != moving).collect();
    let current = session.tabs.iter().position(|tab| tab.id == moving);
    (0..=others.len())
        .filter(|position| Some(*position) != current)
        .filter_map(|position| {
            let label = match position.checked_sub(1) {
                None => format!("before \u{201C}{}\u{201D}", others.first()?.name),
                Some(previous) => format!("after \u{201C}{}\u{201D}", others.get(previous)?.name),
            };
            let mut order: Vec<TabId> = others.iter().map(|tab| tab.id).collect();
            order.insert(position, moving);
            Some((
                label,
                SessionCommand::ReorderTabs {
                    session: session.id,
                    order,
                },
            ))
        })
        .collect()
}

/// The pane a move acts on: the host's focused pane when it is in the
/// selected tab, otherwise the tab's first pane.
fn moving_pane(state: &EngineState, key: &TabKey, tab: &Tab) -> Option<PaneId> {
    let focus = state
        .model()
        .host(&key.host)?
        .focus
        .filter(|focus| tab.panes.iter().any(|pane| pane.id == *focus));
    focus.or_else(|| tab.panes.first().map(|pane| pane.id))
}

/// Every other tab on the host with a pane to place the moving pane beside.
fn move_choices(model: &HostModel, from: TabId, pane: PaneId) -> Vec<(String, SessionCommand)> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter().map(move |tab| (session, tab)))
        .filter(|(_, tab)| tab.id != from)
        .filter_map(|(session, tab)| {
            let target = tab.layout.leaves().last().copied()?;
            Some((
                format!("{} / {}", session.name, tab.name),
                SessionCommand::MovePane {
                    pane,
                    to_tab: tab.id,
                    placement: Placement {
                        target,
                        direction: SplitDirection::Horizontal,
                        before: false,
                    },
                },
            ))
        })
        .collect()
}

/// Even side-by-side and stacked arrangements of a tab's panes, in their
/// current reading order; a single pane has nothing to arrange.
fn layout_choices(tab: &Tab) -> Vec<(String, SessionCommand)> {
    let panes = tab.layout.leaves();
    if panes.len() <= 1 {
        return Vec::new();
    }
    [
        ("side by side", SplitDirection::Horizontal),
        ("stacked", SplitDirection::Vertical),
    ]
    .into_iter()
    .map(|(label, direction)| {
        (
            label.to_owned(),
            SessionCommand::SetLayout {
                tab: tab.id,
                layout: LayoutNode::Split {
                    direction,
                    children: panes
                        .iter()
                        .map(|pane| Weighted {
                            node: LayoutNode::Leaf(*pane),
                            weight: EVEN_WEIGHT,
                        })
                        .collect(),
                },
            },
        )
    })
    .collect()
}

/// A name that no existing name already is: the base itself, or the base
/// followed by the first free number from two.
#[must_use]
pub fn numbered_name<'names>(base: &str, existing: impl Iterator<Item = &'names str>) -> String {
    let taken: std::collections::BTreeSet<&str> = existing.collect();
    if !taken.contains(base) {
        return base.to_owned();
    }
    // Among one more candidate than there are taken names, one is free.
    (1..=taken.len().saturating_add(1))
        .filter_map(|number: usize| number.checked_add(1))
        .map(|number| format!("{base} {number}"))
        .find(|candidate| !taken.contains(candidate.as_str()))
        .unwrap_or_else(|| base.to_owned())
}
