//! Commands that show before the host has agreed to them.
//!
//! What makes a session on the other side of a network feel like one on this
//! machine is that a rename appears when it is typed rather than when the
//! answer comes back. So the six commands whose local effect is beyond doubt —
//! a rename, a close, a reorder — are applied here at once, recorded as
//! pending with the model to put back, and settled by the host's own answer.
//!
//! Creation is not among them, and neither are moving a pane or setting a
//! layout: all three need an identity or an arrangement only the host can
//! mint, and inventing a placeholder to reconcile away a moment later is more
//! flicker on the screen than simply waiting one round trip.
//!
//! The effect applied is not a guess at what the host will do; it is the same
//! change, through the same reconciler, that the host will send back. That is
//! what makes the two agree — and it is applied without advancing the
//! generation, because the authoritative delta that follows carries the number.
//! When the two ever disagree, the delta wins.

use core::time::Duration;
use std::time::Instant;

use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::delta::{Delta, RemovalReason};
use iznik_protocol::identity::{CommandId, PaneId, SessionId, TabId};
use iznik_protocol::model::HostModel;
use iznik_protocol::reconcile::apply_change;

use crate::host::identity::HostId;
use crate::model::{HostView, PendingCommand};
use crate::reduce::Notification;

/// How long a command may go unanswered before it is given up on.
///
/// A screen left showing something the host never agreed to is worse than a
/// visible failure: the person is working against a picture of a machine
/// rather than the machine.
pub const PENDING_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// What became of a submitted command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submission {
    /// The number it was given, which is what the host will answer with.
    pub id: CommandId,
    /// Whether its effect is already showing.
    pub optimistic: bool,
}

/// What an answer did to a pending command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmed {
    /// The host applied it, and the pending entry is retired.
    Applied,
    /// The host refused it, and what was shown has been put back.
    RolledBack,
    /// No command of that number is pending here.
    Unknown,
}

/// Sends a command, showing what it does if what it does is beyond doubt.
///
/// The model as it stood is kept with the pending entry, so a refusal — or an
/// answer that never comes — can put the screen back exactly.
pub fn submit(view: &mut HostView, command: SessionCommand, now: Instant) -> Submission {
    let id = view.mint();
    let rollback = view.model.clone();
    let optimistic = locally(&mut view.model, &command);
    if !optimistic {
        // Nothing was applied, but a half-applied cascade would have left the
        // model between two states; putting it back costs a clone and removes
        // the question.
        view.model = rollback.clone();
    }
    view.record(PendingCommand {
        id,
        command,
        rollback,
        submitted_at: now,
    });
    Submission { id, optimistic }
}

/// Settles a pending command with the host's own answer.
///
/// The host is authoritative: what it applied stands, what it refused is put
/// back, and every command submitted after the one refused is re-applied on
/// the corrected model so that one refusal undoes one thing.
pub fn confirm(view: &mut HostView, command: CommandId, outcome: &CommandOutcome) -> Confirmed {
    match outcome {
        CommandOutcome::Applied { .. } => match view.retire(command) {
            Some(_retired) => Confirmed::Applied,
            None => Confirmed::Unknown,
        },
        CommandOutcome::Rejected { .. } => {
            if roll_back(view, command) {
                Confirmed::RolledBack
            } else {
                Confirmed::Unknown
            }
        }
    }
}

/// Gives up on every command that has gone unanswered for longer than
/// `timeout`, putting back what each of them showed.
///
/// `now` is a parameter and no clock is read, so a case about five seconds
/// takes microseconds.
pub fn expire(
    view: &mut HostView,
    host: &HostId,
    now: Instant,
    timeout: Duration,
) -> Vec<Notification> {
    let Some(cutoff) = now.checked_sub(timeout) else {
        return Vec::new();
    };
    let mut told = Vec::new();
    for command in view.sent_before(cutoff) {
        if roll_back(view, command) {
            told.push(Notification::CommandTimedOut {
                host: host.clone(),
                command,
            });
        }
    }
    told
}

/// Puts back what one pending command showed, and re-applies the ones that
/// came after it.
///
/// Two optimistic commands on one tab are two changes on top of each other, so
/// the model kept with the first is the model from before *both*. Restoring it
/// and re-applying the rest, in order, undoes exactly the one that was refused.
fn roll_back(view: &mut HostView, command: CommandId) -> bool {
    let Some(at) = view.pending.iter().position(|held| held.id == command) else {
        return false;
    };
    let entry = view.pending.remove(at);
    view.model = entry.rollback;
    for later in view.pending.iter_mut().skip(at) {
        later.rollback = view.model.clone();
        let _applied = locally(&mut view.model, &later.command);
    }
    true
}

/// Applies a command's local effect, and says whether it had one.
///
/// A cascade that cannot be applied leaves the model as it was and answers
/// `false`: the command still goes to the host, and the host's own delta will
/// say what happened.
fn locally(model: &mut HostModel, command: &SessionCommand) -> bool {
    let Some(deltas) = effect(model, command) else {
        return false;
    };
    let before = model.clone();
    for delta in &deltas {
        if apply_change(model, delta).is_err() {
            *model = before;
            return false;
        }
    }
    !deltas.is_empty()
}

/// The changes the host will send for a command, when they are ones this can
/// know without asking.
///
/// Read against the host's own registry: a pane going takes its place in the
/// layout with it, and empties a tab and a session behind it.
fn effect(model: &HostModel, command: &SessionCommand) -> Option<Vec<Delta>> {
    match command {
        SessionCommand::RenameSession { session, name } => Some(vec![Delta::SessionRenamed {
            session: *session,
            name: name.clone(),
        }]),
        SessionCommand::RenameTab { tab, name } => Some(vec![Delta::TabRenamed {
            tab: *tab,
            name: name.clone(),
        }]),
        SessionCommand::CloseSession { session } => {
            Some(vec![Delta::SessionRemoved { session: *session }])
        }
        SessionCommand::CloseTab { tab } => Some(closing_a_tab(model, *tab)),
        SessionCommand::ClosePane { pane } => closing_a_pane(model, *pane),
        SessionCommand::ReorderTabs { session, order } => Some(vec![Delta::TabsReordered {
            session: *session,
            order: order.clone(),
        }]),
        // An identity or an arrangement only the host can mint.
        SessionCommand::CreateSession { .. }
        | SessionCommand::CreateTab { .. }
        | SessionCommand::CreatePane { .. }
        | SessionCommand::MovePane { .. }
        | SessionCommand::SetLayout { .. } => None,
    }
}

/// The changes a tab going causes: the tab, and the session it emptied.
fn closing_a_tab(model: &HostModel, tab: TabId) -> Vec<Delta> {
    let mut changes = vec![Delta::TabRemoved { tab }];
    if let Some(session) = emptied_by(model, tab) {
        changes.push(Delta::SessionRemoved { session });
    }
    changes
}

/// The changes a pane going causes: the pane, and then either the layout that
/// no longer places it or the tab it emptied.
fn closing_a_pane(model: &HostModel, pane: PaneId) -> Option<Vec<Delta>> {
    let holding = model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .find(|tab| tab.panes.iter().any(|held| held.id == pane))?;
    let mut changes = vec![Delta::PaneRemoved {
        pane,
        reason: RemovalReason::Closed,
    }];
    match holding.layout.clone().remove_leaf(pane) {
        Some(arranged) => changes.push(Delta::LayoutChanged {
            tab: holding.id,
            layout: arranged,
        }),
        None => changes.extend(closing_a_tab(model, holding.id)),
    }
    Some(changes)
}

/// The session a tab's going would empty, if it would empty one.
fn emptied_by(model: &HostModel, tab: TabId) -> Option<SessionId> {
    let holding = model
        .sessions
        .iter()
        .find(|session| session.tabs.iter().any(|held| held.id == tab))?;
    // The tab is still there as this is asked; it is the last one that leaves
    // nothing behind.
    (holding.tabs.len() == 1).then_some(holding.id)
}
