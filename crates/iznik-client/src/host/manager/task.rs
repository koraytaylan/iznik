//! What one host's own task does, from its first bootstrap to its last.
//!
//! Everything here runs on the manager's runtime, one instance per host, and
//! touches the shared model only under its lock and never across anything that
//! reaches a network. What it is for is in [`super`]: isolation, and a resume
//! that carries a pane's bytes across a dropped link.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_protocol::command::{CommandOutcome, encode_session_command};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::{CHANNEL_CONTROL, ToServer, decode_to_client, encode_to_server};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::bootstrap::bootstrap_watched;
use crate::bootstrap::launch::{BootstrapError, Decision, Stage, expiry, launch};
use crate::commands::{confirm, expire};
use crate::host::identity::HostId;
use crate::host::manager::{ManagerEvent, Order, Shared, bootstrapping, seeded};
use crate::host::state::{Action, HostEvent, HostState, HostStateMachine, UpgradeOffer};
use crate::model::HostView;
use crate::reduce::{Effect, Notification, arrived, reduce};
use crate::transport::Transport;
use crate::transport::channel::{ChannelError, RemoteChannel};

/// How one turn of a host's life ended.
enum Ended {
    /// Somebody asked for it to stop.
    Stopped,
    /// The link went, or was dropped on purpose; the machine says what to do
    /// about it.
    Gone,
}

/// One host's whole life: connect, serve, lose the link, wait, connect again.
pub(super) async fn serve(host: HostId, shared: Arc<Shared>, mut orders: UnboundedReceiver<Order>) {
    let machine = Mutex::new(HostStateMachine::new(seeded(
        &shared.options.backoff,
        &host,
    )));
    let _asked = advance(&shared, &host, &machine, HostEvent::Added);
    loop {
        let Some(channel) = connect(&host, &shared, &machine, &mut orders).await else {
            return;
        };
        match pump(&host, &shared, &machine, &mut orders, channel).await {
            Ended::Stopped => return,
            Ended::Gone => {}
        }
    }
}

/// Moves the host's machine, tells anyone watching when it moved, and gives
/// back what must be done about it.
fn advance(
    shared: &Shared,
    host: &HostId,
    machine: &Mutex<HostStateMachine>,
    event: HostEvent,
) -> Vec<Action> {
    let Ok(mut held) = machine.lock() else {
        return Vec::new();
    };
    let before = held.state().clone();
    let taken = held.on(event, Instant::now());
    let after = held.state().clone();
    drop(held);
    if after != before {
        shared.publish(&ManagerEvent::Moved {
            host: host.clone(),
            state: after,
        });
    }
    taken
}

/// The moment a waiting host is to be tried again, if it is waiting.
fn waiting_until(machine: &Mutex<HostStateMachine>) -> Option<Instant> {
    match machine.lock().ok()?.state() {
        HostState::Reconnecting { retry_at, .. } | HostState::Failed { retry_at, .. } => {
            Some(*retry_at)
        }
        _running => None,
    }
}

/// Waits until `moment`, taking orders meanwhile.
///
/// Answers `false` when the host was told to stop. Orders that need a channel
/// are dropped: there is none, and a keystroke held for a minute and then
/// delivered is worse than one that went nowhere.
async fn hold_until(moment: Instant, orders: &mut UnboundedReceiver<Order>) -> bool {
    loop {
        let waiting = tokio::time::sleep_until(moment.into());
        tokio::select! {
            () = waiting => return true,
            order = orders.recv() => match order {
                None | Some(Order::Stop) => return false,
                // Somebody asked for it now, so the wait is over.
                Some(Order::Reconnect) => return true,
                Some(_ignored) => {}
            },
        }
    }
}

/// Gets a channel to the host, waiting out whatever backoff the machine holds
/// and trying again for as long as it says to.
///
/// Answers `None` when the host was told to stop.
async fn connect(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    orders: &mut UnboundedReceiver<Order>,
) -> Option<RemoteChannel> {
    loop {
        if let Some(moment) = waiting_until(machine) {
            if !hold_until(moment, orders).await {
                let _torn = advance(shared, host, machine, HostEvent::Removed);
                return None;
            }
            let _tried = advance(shared, host, machine, HostEvent::RetryDue);
        }
        match reach(host, shared, machine).await {
            Ok(reached) => return Some(accept(host, shared, machine, reached).await),
            Err(error) => {
                let taken = advance(
                    shared,
                    host,
                    machine,
                    HostEvent::Failed {
                        error: error.to_string(),
                    },
                );
                // A machine that scheduled nothing has nothing more to do.
                if !taken.iter().any(waits) {
                    return None;
                }
            }
        }
    }
}

/// Whether an action is one that schedules another attempt.
fn waits(action: &Action) -> bool {
    matches!(action, Action::RetryAt(_moment))
}

/// What reaching a host got: its channel, what it holds, what its server says
/// it is, and a newer one if this build carries it.
struct Reached {
    /// The channel.
    channel: RemoteChannel,
    /// The model the host answered with.
    snapshot: iznik_protocol::model::HostModel,
    /// What its server says it is.
    version: String,
    /// A newer one, when this build carries one.
    offer: Option<UpgradeOffer>,
}

/// Bootstraps a host and opens a channel to it.
///
/// # Errors
///
/// The [`BootstrapError`] naming the stage that failed.
async fn reach(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
) -> Result<Reached, BootstrapError> {
    let transport = Transport::for_alias(
        &host.0,
        &shared.options.runtime_paths,
        shared.options.ssh.clone(),
    );
    let options = bootstrapping(&shared.options);
    let deadline = shared.options.bootstrap_deadline;
    if host.local_socket().is_some() {
        // A socket on this machine: there is nothing to probe and nothing to
        // install, and `unix:` is the alias this crate owns.
        let (channel, snapshot) = launch(&transport, None, &options, expiry(deadline)).await?;
        let version = channel.greeting().server_version.clone();
        return Ok(Reached {
            channel,
            snapshot,
            version,
            offer: None,
        });
    }
    let watching = |stage: Stage| {
        let _reported = advance(shared, host, machine, HostEvent::Reached { stage });
    };
    let connected =
        bootstrap_watched(&transport, &shared.artifacts, &options, deadline, &watching).await?;
    let offer = match connected.decision {
        Decision::UpgradeAvailable { installed, bundled } => {
            Some(UpgradeOffer { installed, bundled })
        }
        _settled => None,
    };
    let version = connected.channel.greeting().server_version.clone();
    Ok(Reached {
        channel: connected.channel,
        snapshot: connected.snapshot,
        version,
        offer,
    })
}

/// Takes what the host answered into the model, tells the machine it is
/// connected, and resumes every pane the model holds a byte for.
async fn accept(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    reached: Reached,
) -> RemoteChannel {
    let Reached {
        mut channel,
        snapshot,
        version,
        offer,
    } = reached;
    if let Ok(mut model) = shared.model.lock() {
        match model.host_mut(host) {
            Some(view) => view.model = snapshot,
            None => {
                let _first = model.insert(host.clone(), HostView::of(snapshot));
            }
        }
    }
    let taken = advance(
        shared,
        host,
        machine,
        HostEvent::Connected {
            server_version: version,
            upgrade: offer,
        },
    );
    if taken.contains(&Action::Resume) {
        resume(host, shared, &mut channel).await;
    }
    channel
}

/// Asks the host to carry on every subscribed pane from the byte this client
/// holds.
///
/// This is the whole point of keeping a cursor: a person who closed a laptop
/// sees what arrived while it was shut, rather than a fresh screen.
async fn resume(host: &HostId, shared: &Arc<Shared>, channel: &mut RemoteChannel) {
    let held: Vec<(PaneId, Sequence)> = shared
        .model
        .lock()
        .ok()
        .and_then(|model| {
            model.host(host).map(|view| {
                view.subscriptions
                    .iter()
                    .map(|(pane, held)| (*pane, held.cursor))
                    .collect()
            })
        })
        .unwrap_or_default();
    for (pane, from_sequence) in held {
        let _sent = write(
            channel,
            &ToServer::Resume {
                pane,
                from_sequence,
            },
        )
        .await;
    }
}

/// Writes one message on the control channel.
///
/// # Errors
///
/// Whatever the channel says, when the link will not take it.
async fn write(channel: &mut RemoteChannel, message: &ToServer) -> Result<(), ChannelError> {
    let bytes = encode_to_server(message).map_err(ChannelError::Message)?;
    channel.send(CHANNEL_CONTROL, &bytes).await
}

/// One turn of the loop: what arrived, or what was asked for.
enum Turn {
    /// The channel had something, or failed.
    Arrived(Result<crate::transport::channel::Received, ChannelError>),
    /// An order came, or the manager let the host go.
    Ordered(Option<Order>),
}

/// Serves a connected host until its link goes or it is told to stop.
async fn pump(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    orders: &mut UnboundedReceiver<Order>,
    mut channel: RemoteChannel,
) -> Ended {
    loop {
        let now = Instant::now();
        let soon = now
            .checked_add(shared.options.expire_interval)
            .unwrap_or(now);
        // The channel's own read is cancel-safe, so the two may race; the
        // borrow it holds is why the turn is decided before it is acted on.
        let turn = tokio::select! {
            arrived = channel.next(soon) => Turn::Arrived(arrived),
            order = orders.recv() => Turn::Ordered(order),
        };
        match turn {
            Turn::Arrived(Ok(received)) => {
                if !heard(host, shared, &mut channel, received).await {
                    let _dead = advance(shared, host, machine, dead("the link failed"));
                    return Ended::Gone;
                }
            }
            // Nothing arrived inside the wake-up, which is not a failure:
            // the loop goes round so that orders are still taken.
            Turn::Arrived(Err(ChannelError::Deadline { .. })) => {}
            Turn::Arrived(Err(error)) => {
                let _dead = advance(shared, host, machine, dead(&error.to_string()));
                return Ended::Gone;
            }
            Turn::Ordered(None | Some(Order::Stop)) => {
                let _torn = advance(shared, host, machine, HostEvent::Removed);
                return Ended::Stopped;
            }
            Turn::Ordered(Some(Order::Reconnect)) => {
                channel.close();
                let _dead = advance(shared, host, machine, dead("a reconnection was asked for"));
                // Asked for, so the backoff is not waited out.
                let _now = advance(shared, host, machine, HostEvent::RetryDue);
                return Ended::Gone;
            }
            Turn::Ordered(Some(order)) => {
                if carry(&mut channel, order).await.is_err() {
                    let _dead = advance(shared, host, machine, dead("the link would not take it"));
                    return Ended::Gone;
                }
            }
        }
    }
}

/// The event a lost link is.
fn dead(detail: &str) -> HostEvent {
    HostEvent::LinkDead {
        detail: detail.to_owned(),
    }
}

/// Gives up on the commands this host never answered.
pub(super) fn give_up(host: &HostId, shared: &Arc<Shared>) {
    let told = shared
        .with(host, |view| {
            expire(
                view,
                host,
                Instant::now(),
                shared.options.pending_command_timeout,
            )
        })
        .unwrap_or_default();
    for notification in told {
        shared.publish(&ManagerEvent::Notify(notification));
    }
}

/// Takes what arrived into the model and does what it asks for.
///
/// Answers `false` when the channel would not take what had to be written
/// back.
async fn heard(
    host: &HostId,
    shared: &Arc<Shared>,
    channel: &mut RemoteChannel,
    received: crate::transport::channel::Received,
) -> bool {
    let effects = if received.channel == CHANNEL_CONTROL {
        let Ok(message) = decode_to_client(&received.payload) else {
            return true;
        };
        reduce_under(shared, host, &message)
    } else {
        carried(host, shared, &received);
        Vec::new()
    };
    for effect in effects {
        if !act(host, shared, channel, effect).await {
            return false;
        }
    }
    true
}

/// Moves a pane's cursor by what arrived on its channel, and passes the bytes
/// on with the byte position they start at.
fn carried(host: &HostId, shared: &Arc<Shared>, received: &crate::transport::channel::Received) {
    let Ok(mut model) = shared.model.lock() else {
        return;
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
    let _nothing = arrived(&mut model, host, received.channel, received.payload.len());
    drop(model);
    if let Some((pane, sequence)) = standing {
        shared.publish(&ManagerEvent::Bytes {
            host: host.clone(),
            pane,
            sequence,
            bytes: received.payload.clone(),
        });
    }
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
        Effect::ReleaseChannel { channel: number } => {
            write(channel, &ToServer::ChannelReleased { channel: number })
                .await
                .is_ok()
        }
        Effect::Screen { pane, sequence } => {
            shared.publish(&ManagerEvent::Screen {
                host: host.clone(),
                pane,
                sequence,
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
    let Notification::CommandFinished {
        command, outcome, ..
    } = notification
    else {
        return;
    };
    let settled: CommandOutcome = outcome.clone();
    let _confirmed = shared.with(host, |view| confirm(view, *command, &settled));
}

/// Writes one order on the channel.
///
/// # Errors
///
/// Whatever the channel says, when the link will not take it.
async fn carry(channel: &mut RemoteChannel, order: Order) -> Result<(), ChannelError> {
    let message = match order {
        Order::Subscribe { pane } => ToServer::Subscribe { pane },
        Order::Unsubscribe { pane } => ToServer::Unsubscribe { pane },
        Order::Input { pane, bytes } => ToServer::Input { pane, bytes },
        Order::Resize {
            pane,
            columns,
            rows,
        } => ToServer::Resize {
            pane,
            columns,
            rows,
        },
        Order::Focus { pane: Some(pane) } => ToServer::Focus { pane },
        Order::Credit {
            channel: number,
            bytes,
        } => ToServer::Credit {
            channel: number,
            bytes,
        },
        Order::Command { id, command } => ToServer::Command {
            command_id: id,
            payload: encode_session_command(&command).map_err(ChannelError::Message)?,
        },
        // The first two are the loop's own business; the third says the
        // person is looking at no pane at all, which the host is not told
        // because there is nothing for it to prefer.
        Order::Reconnect | Order::Stop | Order::Focus { pane: None } => return Ok(()),
    };
    write(channel, &message).await
}
