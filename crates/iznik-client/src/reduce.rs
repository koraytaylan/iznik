//! The server's messages applied to the client's model.
//!
//! One function, no I/O, and every consequence returned as a value for the
//! manager to act on: ask for a snapshot, let a channel go, redraw from a
//! screen, tell somebody. That shape is what makes the interesting property
//! cheap — the model this arrives at is the model the server holds, and it is
//! proven over a thousand generated sequences in under a second.
//!
//! Convergence is not re-proven here so much as inherited: the reconciler this
//! calls is the protocol's own, the same code plan 0003 proved the server's
//! deltas against, so a client that applies them arrives where the server is
//! by construction. What is proven here is the rest of it — that the routing
//! is by host and happens first, that a gap in the numbering asks rather than
//! guesses, and that a subscription's cursor tracks the bytes.

use iznik_protocol::command::{CommandOutcome, decode_command_outcome};
use iznik_protocol::delta::decode_delta;
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence};
use iznik_protocol::message::{ErrorCode, MarkKind, ToClient};
use iznik_protocol::model::decode_host_model;
use iznik_protocol::reconcile::{ReconcileError, apply};

use crate::commands::replay;
use crate::host::identity::HostId;
use crate::model::{ClientModel, HostView};

/// Something the manager must do about a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Ask the host for its whole model, because this one can no longer be
    /// brought up to date by numbering.
    RequestSnapshot,
    /// The host has stopped sending a pane's output on this channel, so the
    /// number may be given back.
    ReleaseChannel {
        /// The channel.
        channel: u8,
    },
    /// The host sent a screen for a pane; whatever is drawn must be replaced
    /// with it, and the stream picked up from `sequence`.
    Screen {
        /// The pane.
        pane: PaneId,
        /// The byte the screen is exact at.
        sequence: Sequence,
        /// Its width in cells.
        columns: u16,
        /// Its height in cells.
        rows: u16,
        /// The bytes that reproduce it.
        bytes: Vec<u8>,
    },
    /// Commands a host that is gone answered, which stop being shown.
    ///
    /// An effect rather than a line written here: this module returns what it
    /// did and writes nothing, and the model's lock is held for the whole of
    /// a reduction — so a log written from inside it would be a file being
    /// waited on by every caller who wanted the model.
    Abandoned {
        /// Their numbers, in the order they were sent.
        commands: Vec<CommandId>,
    },
    /// Something a person, or the layer above, is told.
    Notify(Notification),
}

/// Something worth telling whoever is watching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notification {
    /// A command this client sent was answered.
    CommandFinished {
        /// The host that answered.
        host: HostId,
        /// This client's number for the command.
        command: CommandId,
        /// What the host did about it.
        outcome: CommandOutcome,
    },
    /// A command this client sent was never answered.
    ///
    /// Yielded by `commands::expire` rather than here: a UI element left in a
    /// state the server never agreed to is worse than a visible failure.
    CommandTimedOut {
        /// The host that did not answer.
        host: HostId,
        /// This client's number for the command.
        command: CommandId,
    },
    /// A shell-integration event in a pane.
    Mark {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
        /// Where in the pane's stream it sits.
        sequence: Sequence,
        /// What happened.
        kind: MarkKind,
    },
    /// The host refused something.
    Refused {
        /// The host.
        host: HostId,
        /// Why.
        code: ErrorCode,
        /// Its words.
        message: String,
    },
    /// The host said something this client could not read.
    Malformed {
        /// The host.
        host: HostId,
        /// What could not be read.
        detail: String,
    },
}

/// Applies one message from `host` to the model, and says what must follow.
///
/// Routing happens before anything else: a message names a host, and nothing
/// it does can reach another host's view. A message for a host this client
/// does not know is dropped — except a snapshot, which is what makes a host
/// known.
pub fn reduce(model: &mut ClientModel, host: &HostId, message: &ToClient) -> Vec<Effect> {
    if let ToClient::Snapshot {
        generation,
        payload,
    } = message
    {
        return replace(model, host, *generation, payload);
    }
    let Some(view) = model.host_mut(host) else {
        return Vec::new();
    };
    match message {
        ToClient::Delta {
            generation,
            payload,
        } => reconcile(view, host, *generation, payload),
        ToClient::PaneChannel {
            pane,
            channel,
            sequence,
        } => {
            let _opened = view.subscribe(*pane, *channel, *sequence);
            Vec::new()
        }
        ToClient::PaneDetached { pane, channel } => {
            let _dropped = view.unsubscribe(*pane);
            vec![Effect::ReleaseChannel { channel: *channel }]
        }
        ToClient::Screen {
            pane,
            sequence,
            columns,
            rows,
            bytes,
        } => {
            if let Some(held) = view.subscription_mut(*pane) {
                held.resume_at(*sequence);
            }
            vec![Effect::Screen {
                pane: *pane,
                sequence: *sequence,
                columns: *columns,
                rows: *rows,
                bytes: bytes.clone(),
            }]
        }
        ToClient::CommandResult {
            command_id,
            payload,
        } => vec![Effect::Notify(answered(host, *command_id, payload))],
        ToClient::Mark {
            pane,
            sequence,
            kind,
        } => vec![Effect::Notify(Notification::Mark {
            host: host.clone(),
            pane: *pane,
            sequence: *sequence,
            kind: kind.clone(),
        })],
        ToClient::Error { code, message } => vec![Effect::Notify(Notification::Refused {
            host: host.clone(),
            code: *code,
            message: message.clone(),
        })],
        // The handshake is the channel's, the liveness is the channel's, and a
        // snapshot was taken before the lookup above.
        ToClient::Hello { .. } | ToClient::Pong | ToClient::Snapshot { .. } => Vec::new(),
    }
}

/// Bytes that arrived on a pane's own channel.
///
/// Not a [`ToClient`]: a pane's output is raw on the channel the host
/// announced for it, and only how much of it there was matters to the model.
/// The cursor is what a resume asks from, so every byte that arrives must move
/// it — which is why this exists beside [`reduce`] rather than inside it.
pub fn arrived(model: &mut ClientModel, host: &HostId, channel: u8, bytes: usize) -> Vec<Effect> {
    let Some(view) = model.host_mut(host) else {
        return Vec::new();
    };
    let carried = u64::try_from(bytes).unwrap_or(u64::MAX);
    // The one pane whose bytes these are. Advancing every subscription that
    // holds the number would move a cursor for a pane that sent nothing, and
    // that pane would then resume from a byte it never reached — losing
    // exactly the output a resume exists to keep.
    let Some(pane) = view.carrying(channel) else {
        return Vec::new();
    };
    if let Some(held) = view.subscription_mut(pane) {
        let _reached = held.advance(carried);
        held.spend(carried);
    }
    Vec::new()
}

/// A snapshot: the host's whole model, replacing whatever was there.
fn replace(
    model: &mut ClientModel,
    host: &HostId,
    generation: Generation,
    payload: &[u8],
) -> Vec<Effect> {
    let held = match decode_host_model(payload) {
        Ok(held) => held,
        Err(source) => {
            return vec![Effect::Notify(Notification::Malformed {
                host: host.clone(),
                detail: format!("the host's model could not be read: {source}"),
            })];
        }
    };
    // The generation the message carries is the model's; a snapshot that
    // disagreed with itself would leave every later delta unapplicable.
    let mut replaced = held;
    replaced.generation = generation;
    match model.host_mut(host) {
        Some(view) => {
            // The subscriptions, the focus and the commands in flight are this
            // client's own and outlive a snapshot; only the host's model is
            // replaced by it — and what is still in flight goes back on top,
            // because a snapshot is what the host has said and not what this
            // client has asked for.
            let commands = view.settle(replaced);
            replay(view);
            if !commands.is_empty() {
                return vec![Effect::Abandoned { commands }];
            }
        }
        None => {
            let _first = model.insert(host.clone(), HostView::of(replaced));
        }
    }
    Vec::new()
}

/// A delta: one numbered change, through the protocol's own reconciler.
fn reconcile(
    view: &mut HostView,
    host: &HostId,
    generation: Generation,
    payload: &[u8],
) -> Vec<Effect> {
    let delta = match decode_delta(payload) {
        Ok(delta) => delta,
        Err(source) => return unreadable(host, &format!("a change could not be read: {source}")),
    };
    // Against what the host last said, not against what this client is
    // showing: a change already applied optimistically would be refused as one
    // the model cannot take, and a snapshot would be asked for after every
    // close that worked.
    let mut standing = view.settled.clone();
    match apply(&mut standing, generation, &delta) {
        Ok(()) => {
            view.settle(standing);
            replay(view);
            Vec::new()
        }
        // A number was missed. Nothing is guessed and nothing is applied: the
        // host is asked for the whole of it, which is the one recovery that
        // cannot be wrong.
        Err(ReconcileError::GenerationGap { .. }) => vec![Effect::RequestSnapshot],
        // The change did not fit the model this client holds, and `apply`
        // leaves it exactly as it was — so the two have diverged, and only a
        // snapshot settles which is right.
        Err(source) => unreadable(host, &format!("a change did not fit: {source}")),
    }
}

/// What is done about something the host said that this could not use.
fn unreadable(host: &HostId, detail: &str) -> Vec<Effect> {
    vec![
        Effect::Notify(Notification::Malformed {
            host: host.clone(),
            detail: detail.to_owned(),
        }),
        Effect::RequestSnapshot,
    ]
}

/// The answer to a command, read into what it says.
fn answered(host: &HostId, command: CommandId, payload: &[u8]) -> Notification {
    match decode_command_outcome(payload) {
        Ok(outcome) => Notification::CommandFinished {
            host: host.clone(),
            command,
            outcome,
        },
        Err(source) => Notification::Malformed {
            host: host.clone(),
            detail: format!(
                "the answer to command {} could not be read: {source}",
                command.0
            ),
        },
    }
}
