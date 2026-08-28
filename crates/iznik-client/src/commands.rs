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
//!
//! Two models are kept apart to make "the delta always wins" true rather than
//! hopeful. What the host has said is one model; what this client is showing
//! is that model with everything still in flight applied on top. A delta from
//! the host is applied to the first and the pending effects are put back over
//! it, so the host never has to reconcile a change it has not made, and a
//! refusal never undoes one it has.
//!
//! "In flight" reaches one frame further than it sounds. The host answers a
//! command and *then* announces the change, so between those two frames the
//! command is answered and its effect is still nobody's but this client's. It
//! stays applied until the model reaches the generation the answer named.

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
    let optimistic = locally(&mut view.model, &command);
    view.record(PendingCommand {
        id,
        command,
        answered: None,
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
        CommandOutcome::Applied { generation, .. } => {
            // Not retired: what it did is showing and the host has not
            // announced it yet, so it stays until a delta or a snapshot brings
            // the model to the generation the host says it reached.
            match view
                .pending
                .iter_mut()
                .find(|held| held.id == command && held.answered.is_none())
            {
                Some(held) => {
                    held.answered = Some(*generation);
                    Confirmed::Applied
                }
                None => Confirmed::Unknown,
            }
        }
        CommandOutcome::Rejected { .. } => {
            if roll_back(view, command) {
                Confirmed::RolledBack
            } else {
                Confirmed::Unknown
            }
        }
    }
}

/// Takes back a command that never left this machine, putting back whatever
/// it showed.
///
/// Answers whether there was one. What separates this from a refusal is who
/// refused: nothing on the host knows about a command whose channel would not
/// take it, so nothing is waited for and nobody is told.
pub fn withdraw(view: &mut HostView, command: CommandId) -> bool {
    roll_back(view, command)
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

/// Says, on the record, which commands stopped being shown because the host
/// that answered them is gone.
///
/// Not an event: what happened is that this host is another daemon, which the
/// state and the snapshot beside it already say. It is written down because a
/// command that was applied and is no longer shown is the kind of thing
/// somebody reads a log to understand — and because it is said wherever a
/// model is settled, not only where a connection begins.
pub fn abandoned(host: &HostId, commands: &[CommandId]) {
    if commands.is_empty() {
        return;
    }
    tracing::info!(
        host = %host.0,
        commands = ?commands,
        "a replaced daemon answered these, and they stop being shown"
    );
}

/// Applies every command still in flight on top of what the host has said,
/// and refreshes what each of them would be rolled back to.
///
/// This is what keeps the two truths apart. The host's own changes are applied
/// to the settled model, and what this client is still waiting on is put back
/// on top afterwards — so an authoritative delta never has to be reconciled
/// against a change the host has not made yet, and a refusal never has to undo
/// a change the host *has* made.
pub fn replay(view: &mut HostView) {
    view.model = view.settled.clone();
    let reached = view.settled.generation;
    let mut standing = std::mem::take(&mut view.pending);
    // A command the host answered *and* has now announced is the host's own:
    // it is in the settled model, and keeping it here would apply it twice.
    standing.retain(|held| held.answered.is_none_or(|at| at > reached));
    for held in &standing {
        // The rest go back on top in the order they were sent — those still
        // waiting for an answer, and those answered whose change has not
        // arrived yet, which is the window between the two frames the host
        // sends for one command.
        let _applied = locally(&mut view.model, &held.command);
    }
    view.pending = standing;
}

/// Puts back what one pending command showed, and re-applies the ones that
/// came after it.
///
/// Two optimistic commands on one tab are two changes on top of each other, so
/// the model kept with the first is the model from before *both*. Restoring it
/// and re-applying the rest, in order, undoes exactly the one that was refused.
fn roll_back(view: &mut HostView, command: CommandId) -> bool {
    let Some(at) = view
        .pending
        .iter()
        .position(|held| held.id == command && held.answered.is_none())
    else {
        return false;
    };
    let _gone = view.pending.remove(at);
    // Everything the host has said, with everything still in flight put back
    // on top of it: one command's effect is undone by not applying it again.
    replay(view);
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
