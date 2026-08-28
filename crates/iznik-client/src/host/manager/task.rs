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
use crate::commands::{confirm, expire, replay};
use crate::host::identity::HostId;
use crate::host::manager::{ManagerEvent, ORDERS_PER_TURN, Order, Shared, bootstrapping, seeded};
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
    // What was asked for while there was nowhere to send it. A keystroke held
    // for a minute and then delivered is worse than one that went nowhere; a
    // subscription is not, because nothing will ever ask for it again and a
    // pane nobody subscribed to is a pane that stays blank.
    let mut kept: Vec<Order> = Vec::new();
    loop {
        let Some(channel) = connect(&host, &shared, &machine, &mut orders, &mut kept).await else {
            // Nothing more will be tried, and whoever is watching is told so
            // rather than left with a host that simply stopped saying
            // anything.
            shared.publish(&ManagerEvent::Removed { host });
            return;
        };
        match pump(&host, &shared, &machine, &mut orders, &mut kept, channel).await {
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
        // Written down as well as announced. An application is told so it can
        // show it; the log is what somebody reads afterwards, when what they
        // want to know is when a host went and how long it was gone — and it
        // is the only account of that a native application can hand over
        // without having kept one itself.
        tracing::info!(host = %host.0, from = %before, to = %after, "a host moved");
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
async fn hold_until(
    moment: Instant,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
) -> bool {
    loop {
        let waiting = tokio::time::sleep_until(moment.into());
        tokio::select! {
            () = waiting => return true,
            order = orders.recv() => match order {
                None | Some(Order::Stop) => return false,
                // Somebody asked for it now, so the wait is over.
                Some(Order::Reconnect) => return true,
                Some(held) => keep(kept, held),
            },
        }
    }
}

/// Holds on to an order worth asking for again once there is a link, and lets
/// the rest go.
///
/// What is worth keeping is what nothing will ask for twice: a subscription, a
/// size, which pane has the person's attention. Keystrokes and credit are not
/// — a keystroke delivered a minute late is worse than one that went nowhere,
/// and credit belongs to a channel that no longer exists.
fn keep(kept: &mut Vec<Order>, order: Order) {
    match order {
        Order::Subscribe { pane } => {
            kept.retain(|held| !about(held, pane));
            kept.push(Order::Subscribe { pane });
        }
        // Kept, not merely cancelling: the resume that follows a
        // reconnection asks for every pane the model still holds, so a pane
        // somebody let go of while the host was down would come back
        // uninvited unless the letting go is asked for too.
        Order::Unsubscribe { pane } => {
            kept.retain(|held| !about(held, pane));
            kept.push(Order::Unsubscribe { pane });
        }
        // The latest size and the latest focus, and only those: what is kept
        // is a state to arrive at, not a history to replay, and a person
        // moving between panes for an hour on a host that is down must not
        // grow this without end.
        Order::Resize { pane, .. } => {
            kept.retain(
                |held| !matches!(held, Order::Resize { pane: named, .. } if *named == pane),
            );
            kept.push(order);
        }
        Order::Focus { .. } => {
            kept.retain(|held| !matches!(held, Order::Focus { .. }));
            kept.push(order);
        }
        Order::Input { .. }
        | Order::Credit { .. }
        | Order::Command { .. }
        | Order::Screen { .. }
        | Order::Reconnect
        | Order::Stop => {}
    }
}

/// Whether a held order is one pane's subscription, which a later one about
/// the same pane replaces.
///
/// A size is not: a pane subscribed again is still the size it was told.
fn about(order: &Order, pane: PaneId) -> bool {
    match order {
        Order::Subscribe { pane: named } | Order::Unsubscribe { pane: named } => *named == pane,
        _otherwise => false,
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
    kept: &mut Vec<Order>,
) -> Option<RemoteChannel> {
    loop {
        if let Some(moment) = waiting_until(machine) {
            if !hold_until(moment, orders, kept).await {
                let _torn = advance(shared, host, machine, HostEvent::Removed);
                return None;
            }
            let _tried = advance(shared, host, machine, HostEvent::RetryDue);
        }
        match reach(host, shared, machine).await {
            Ok(reached) => {
                let mut channel = accept(host, shared, machine, reached).await;
                // Everything asked for while there was nowhere to send it.
                // What the link would not take stays held: the next
                // connection is the one that will carry it, and a write that
                // failed is a link that has just died.
                let standing = std::mem::take(kept);
                let mut sent = 0_usize;
                for order in &standing {
                    if carry(&mut channel, order.clone()).await.is_err() {
                        break;
                    }
                    sent = sent.saturating_add(1);
                }
                kept.extend(standing.into_iter().skip(sent));
                return Some(channel);
            }
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
    // The snapshot a connection begins with is taken inside the launch, before
    // there is a loop to hear it in, so it is announced from here: what is
    // above the manager is told about a host's model the same way whether the
    // model arrived with the connection or after it.
    told_the_model(host, shared, &snapshot);
    let mut abandoned = Vec::new();
    if let Ok(mut model) = shared.model.lock() {
        match model.host_mut(host) {
            Some(view) => {
                // What the host says replaces what it said before, and
                // whatever is still in flight goes back on top: a command
                // whose answer was lost with the link is still this client's
                // to show, and its rollback must be the model that came back
                // rather than the one from before the drop.
                abandoned = view.settle(snapshot);
                replay(view);
            }
            None => {
                let _first = model.insert(host.clone(), HostView::of(snapshot));
            }
        }
    }
    if !abandoned.is_empty() {
        // Not an event: what happened is that this host is another daemon,
        // which the state and the snapshot beside it already say. It is
        // written down because a command that was applied and is no longer
        // shown is the kind of thing somebody reads a log to understand.
        tracing::info!(
            host = %host.0,
            commands = ?abandoned,
            "a replaced daemon answered these, and they stop being shown"
        );
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
    kept: &mut Vec<Order>,
    mut channel: RemoteChannel,
) -> Ended {
    let mut carried = 0_usize;
    loop {
        let now = Instant::now();
        let soon = now
            .checked_add(shared.options.expire_interval)
            .unwrap_or(now);
        let turn = next_turn(&mut channel, orders, soon, carried < ORDERS_PER_TURN).await;
        carried = if matches!(turn, Turn::Ordered(_)) {
            carried.saturating_add(1)
        } else {
            0
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
                if carry(&mut channel, order.clone()).await.is_err() {
                    // Held for the next connection, under the same rule as an
                    // order that arrived while there was none: what the link
                    // died holding was taken from the application, which was
                    // told so, and dropping it here would lose a subscription
                    // and leave a pane blank for ever.
                    keep(kept, order);
                    let _dead = advance(shared, host, machine, dead("the link would not take it"));
                    return Ended::Gone;
                }
            }
        }
    }
}

/// The next turn: an order that is already waiting, or whatever the link says.
///
/// Orders come first while `ordering`, because a burst of them must not be
/// paced by what the host happens to be saying: a hundred keystrokes handed
/// over at once are a hundred orders, and hearing between each of them would
/// put a round trip between one and the next. After enough in a row the link
/// is read first instead, so that a caller who never stops ordering cannot
/// keep this loop from hearing.
///
/// The channel's own read is cancel-safe, so the two may race; the borrow it
/// holds is why the turn is decided here and acted on by the caller.
async fn next_turn(
    channel: &mut RemoteChannel,
    orders: &mut UnboundedReceiver<Order>,
    soon: Instant,
    ordering: bool,
) -> Turn {
    if ordering {
        tokio::select! {
            biased;
            order = orders.recv() => Turn::Ordered(order),
            arrived = channel.next(soon) => Turn::Arrived(arrived),
        }
    } else {
        tokio::select! {
            biased;
            arrived = channel.next(soon) => Turn::Arrived(arrived),
            order = orders.recv() => Turn::Ordered(order),
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
        let taken = reduce_under(shared, host, &message);
        // What the host said about its model goes on as the host said it: the
        // layer above this one hands those bytes to an application that
        // decodes them with the protocol's own reader.
        //
        // Except a change this client could not take. A gap in the numbering
        // or a change that did not fit leaves the model exactly as it was and
        // asks for the whole of it; passing the change on regardless would
        // have the application apply what this client refused, and the two
        // would part until the snapshot arrived. What is passed on is what
        // was applied, so an application that applies every change in turn
        // holds what this client holds.
        if taken.is_empty() || !matches!(message, iznik_protocol::message::ToClient::Delta { .. }) {
            announced(host, shared, &message);
        }
        taken
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

/// Announces a whole model, in the encoding the host itself uses.
fn told_the_model(host: &HostId, shared: &Arc<Shared>, model: &iznik_protocol::model::HostModel) {
    let Ok(payload) = iznik_protocol::model::encode_host_model(model) else {
        return;
    };
    shared.publish(&ManagerEvent::Snapshot {
        host: host.clone(),
        generation: model.generation,
        payload,
    });
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
        Order::Screen { pane } => ToServer::ScreenRequest { pane },
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
