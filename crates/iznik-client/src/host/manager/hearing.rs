//! What a host's task does with what its host says: the model moved, a
//! pane's bytes passed on with the credit they earn, and whatever the host
//! asked for written back.

use std::sync::Arc;

use iznik_protocol::command::CommandOutcome;
use iznik_protocol::message::{CHANNEL_CONTROL, ToServer, decode_to_client};

use crate::commands::{abandoned, confirm, withdraw};
use crate::host::identity::HostId;
use crate::host::manager::credit::{MAXIMUM_UNRETURNED_BYTES, Undeliverable};
use crate::host::manager::task::write;
use crate::host::manager::{ManagerEvent, Shared};
use crate::reduce::{Effect, Notification, arrived, reduce};
use crate::transport::channel::RemoteChannel;

/// Takes what arrived into the model and does what it asks for.
///
/// # Errors
///
/// What to say about a link that has to go: the channel would not take what
/// had to be written back, or the host broke flow control.
pub(super) async fn heard(
    host: &HostId,
    shared: &Arc<Shared>,
    channel: &mut RemoteChannel,
    received: crate::transport::channel::Received,
) -> Result<(), String> {
    let effects = if received.channel == CHANNEL_CONTROL {
        let message = match decode_to_client(&received.payload) {
            Ok(message) => message,
            // Not a reason to drop the link — a newer server may say something
            // this build has no word for — but not a reason to say nothing
            // either: a message that was lost is the explanation for whatever
            // looks wrong next.
            Err(refusal) => {
                tracing::warn!(
                    host = ?host.0,
                    %refusal,
                    bytes = received.payload.len(),
                    "a control message could not be read, and was passed over"
                );
                return Ok(());
            }
        };
        let taken = reduce_under(shared, host, &message);
        if let Ok(mut credit) = shared.credit.lock() {
            match &message {
                iznik_protocol::message::ToClient::PaneChannel {
                    pane,
                    channel: number,
                    ..
                } => {
                    credit.open(host, *pane, *number);
                }
                iznik_protocol::message::ToClient::PaneDetached { pane, .. } => {
                    credit.detach(host, *pane);
                }
                _ => {}
            }
        }
        // What the host said about its model goes on as the host said it: the
        // layer above this one hands those bytes to an application that
        // decodes them with the protocol's own reader.
        //
        // Except what this client could not take. A gap in the numbering, a
        // change that did not fit, a model that could not be read: each leaves
        // the model exactly as it was, and each asks for something. Passing it
        // on regardless would have the application take what this client
        // refused, and the two would part until the snapshot arrived. What is
        // passed on is what was applied, so an application that applies every
        // one in turn holds what this client holds.
        if !refused(&taken) || !carries_a_model(&message) {
            announced(host, shared, &message);
        }
        taken
    } else {
        carried(host, shared, &received)?;
        Vec::new()
    };
    for effect in effects {
        if !act(host, shared, channel, effect).await {
            return Err("the link would not take what had to be written back".to_owned());
        }
    }
    Ok(())
}

/// Moves a pane's cursor by what arrived on its channel, and passes the bytes
/// on with the byte position they start at.
///
/// # Errors
///
/// What to say about a link that has to go: the model's lock broken, or a
/// host that sent a pane more than it may have outstanding — which is not a
/// delivery but a host out of flow control, and taking it would hold without
/// bound whatever it chose to send.
fn carried(
    host: &HostId,
    shared: &Arc<Shared>,
    received: &crate::transport::channel::Received,
) -> Result<(), String> {
    let Ok(mut model) = shared.model.lock() else {
        return Err("the model's lock is broken".to_owned());
    };
    // The byte these start at is the cursor *before* they are counted.
    let standing = model
        .host(host)
        .and_then(|view| view.carrying(received.channel))
        .and_then(|pane| {
            model
                .host(host)
                .and_then(|view| view.subscription(pane))
                .map(|held| (pane, held.cursor))
        });
    let Some((pane, sequence)) = standing else {
        return Ok(());
    };
    let bytes = u32::try_from(received.payload.len())
        .map_err(|_too_many| "a frame longer than a frame may be arrived".to_owned())?;
    let delivered = shared
        .credit
        .lock()
        .map_err(|_broken| "the credit lock is broken".to_owned())?
        .deliver(host, pane, bytes);
    let receipt = match delivered {
        Ok(receipt) => receipt,
        Err(Undeliverable::NoStream) => return Ok(()),
        Err(Undeliverable::Overrun { unreturned }) => {
            let detail = format!(
                "{host} sent pane {} past its credit: {unreturned} bytes would be outstanding, \
                 more than the {MAXIMUM_UNRETURNED_BYTES} any window allows",
                pane.0
            );
            tracing::warn!(host = ?host.0, pane = pane.0, unreturned, "a host sent past its credit");
            return Err(detail);
        }
    };
    let _nothing = arrived(&mut model, host, received.channel, received.payload.len());
    drop(model);
    shared.publish(&ManagerEvent::Bytes {
        host: host.clone(),
        pane,
        sequence,
        bytes: received.payload.clone(),
        receipt: Some(receipt),
    });
    Ok(())
}

/// Announces a whole model, in the encoding the host itself uses.
pub(super) fn told_the_model(
    host: &HostId,
    shared: &Arc<Shared>,
    model: &iznik_protocol::model::HostModel,
) {
    let Ok(payload) = iznik_protocol::model::encode_host_model(model) else {
        return;
    };
    shared.publish(&ManagerEvent::Snapshot {
        host: host.clone(),
        generation: model.generation,
        payload,
    });
}

/// Whether what came back from a reduction says the message was not taken.
///
/// The two ways a message carrying a model is refused: a number that did not
/// follow the last, which asks for the whole of it, and bytes that could not
/// be read, which say so. Anything else came back from a message that *was*
/// taken — the account of what a replaced daemon answered among them, which a
/// snapshot that was applied perfectly well produces.
fn refused(taken: &[Effect]) -> bool {
    taken.iter().any(|effect| {
        matches!(
            effect,
            Effect::RequestSnapshot | Effect::Notify(Notification::Malformed { .. })
        )
    })
}

/// Whether a message is one of the two that say what the host's model is.
fn carries_a_model(message: &iznik_protocol::message::ToClient) -> bool {
    matches!(
        message,
        iznik_protocol::message::ToClient::Snapshot { .. }
            | iznik_protocol::message::ToClient::Delta { .. }
    )
}

/// Passes on the host's own account of its model, unchanged.
fn announced(host: &HostId, shared: &Arc<Shared>, message: &iznik_protocol::message::ToClient) {
    let told = match message {
        iznik_protocol::message::ToClient::Snapshot {
            generation,
            payload,
        } => ManagerEvent::Snapshot {
            host: host.clone(),
            generation: *generation,
            payload: payload.clone(),
        },
        iznik_protocol::message::ToClient::Delta {
            generation,
            payload,
        } => ManagerEvent::Delta {
            host: host.clone(),
            generation: *generation,
            payload: payload.clone(),
        },
        iznik_protocol::message::ToClient::PaneDetached { pane, .. } => ManagerEvent::Detached {
            host: host.clone(),
            pane: *pane,
        },
        _otherwise => return,
    };
    shared.publish(&told);
}

/// Applies one message to the model, with the lock held for that and nothing
/// else.
fn reduce_under(
    shared: &Arc<Shared>,
    host: &HostId,
    message: &iznik_protocol::message::ToClient,
) -> Vec<Effect> {
    let Ok(mut model) = shared.model.lock() else {
        return Vec::new();
    };
    reduce(&mut model, host, message)
}

/// Does what one effect asks for.
///
/// Answers `false` when the channel would not take what it had to write.
async fn act(
    host: &HostId,
    shared: &Arc<Shared>,
    channel: &mut RemoteChannel,
    effect: Effect,
) -> bool {
    match effect {
        Effect::RequestSnapshot => write(channel, &ToServer::SnapshotRequest).await.is_ok(),
        // Written down here, where the model's lock is not held: a log is a
        // file, and a file is something every other caller would be waiting
        // on if it were written from inside a reduction.
        Effect::Abandoned { commands } => {
            abandoned(host, &commands);
            true
        }
        Effect::ReleaseChannel { channel: number } => {
            write(channel, &ToServer::ChannelReleased { channel: number })
                .await
                .is_ok()
        }
        Effect::Screen {
            pane,
            sequence,
            columns,
            rows,
            bytes,
        } => {
            shared.publish(&ManagerEvent::Screen {
                host: host.clone(),
                pane,
                sequence,
                columns,
                rows,
                bytes,
            });
            true
        }
        Effect::Notify(notification) => {
            settle(host, shared, &notification);
            shared.publish(&ManagerEvent::Notify(notification));
            true
        }
    }
}

/// Retires or rolls back the pending command an answer settles.
fn settle(host: &HostId, shared: &Arc<Shared>, notification: &Notification) {
    match notification {
        Notification::CommandFinished {
            command, outcome, ..
        } => {
            let settled: CommandOutcome = outcome.clone();
            let _confirmed = shared.with(host, |view| confirm(view, *command, &settled));
        }
        // An answer that came and could not be read settles the command as
        // surely as a refusal: what it showed is put back now.
        Notification::CommandUnreadable { command, .. } => {
            let _taken_back = shared.with(host, |view| withdraw(view, *command));
        }
        _otherwise => {}
    }
}
