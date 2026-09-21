//! What one host's own task does, from its first bootstrap to its last.
//!
//! Everything here runs on the manager's runtime, one instance per host, and
//! touches the shared model only under its lock and never across anything that
//! reaches a network. What it is for is in [`super`]: isolation, and a resume
//! that carries a pane's bytes across a dropped link.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, encode_session_command};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::{CHANNEL_CONTROL, ToServer, decode_to_client, encode_to_server};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::bootstrap::launch::{BootstrapError, Decision, Stage, bundled, expiry, launch};
use crate::bootstrap::probe::InstalledServer;
use crate::bootstrap::{bootstrap_watched, upgrade};
use crate::commands::{abandoned, confirm, expire, replay};
use crate::host::identity::HostId;
use crate::host::manager::{ManagerEvent, ORDERS_PER_TURN, Order, Shared, bootstrapping, seeded};
use crate::host::state::{
    Action, HostEvent, HostState, HostStateMachine, UpgradeOffer, UpgradeReason,
};
use crate::model::HostView;
use crate::reduce::{Effect, Notification, arrived, reduce};
use crate::transport::Transport;
use crate::transport::channel::{ChannelError, RemoteChannel, ServerHello};

/// How one turn of a host's life ended.
enum Ended {
    /// Somebody asked for it to stop.
    Stopped,
    /// The link went, or was dropped on purpose; the machine says what to do
    /// about it.
    Gone,
    /// Somebody asked for this host's server to be replaced with this build's.
    ///
    /// Answered rather than acted on where it was heard, because the
    /// replacement stops the daemon the link is talking to: the outer loop is
    /// what closes the channel and runs it, and the order queue stays open
    /// across it, so what a still-drawing window asks for meanwhile is held
    /// for the new link instead of being refused.
    Upgrade {
        /// Whether to replace it even though it holds panes, which ends them.
        force: bool,
    },
}

/// One host's whole life: connect, serve, lose the link, wait, connect again —
/// and replace the server when somebody asks, without ever not being held.
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
            Ended::Upgrade { force } => {
                replace(&host, &shared, &machine, force).await;
                // The alias is held throughout: the loop goes straight back to
                // connecting, and everything asked for during the replacement
                // is already on this task's queue.
            }
        }
    }
}

/// Replaces the server on a host from inside its own task.
///
/// The task stays alive and keeps taking orders while this runs, so the host
/// is never un-held: an order that arrives now waits on the queue and is
/// carried once the new link is up. A refusal is said aloud, because an upgrade
/// that did not happen must not look like one that did.
async fn replace(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    force: bool,
) {
    let transport = Transport::for_alias(
        &host.0,
        &shared.options.runtime_paths,
        shared.options.ssh.clone(),
    );
    let replaced = upgrade(
        &transport,
        &shared.artifacts,
        &bootstrapping(&shared.options),
        force,
        shared.options.bootstrap_deadline,
    )
    .await;
    let error = match replaced {
        Ok(()) => None,
        Err(refusal) => Some(format!("upgrading {host}: {refusal}")),
    };
    // The next `connect` is what reaches the new server; say the failure now
    // and let the reconnect that follows say the rest.
    if let Some(detail) = error {
        let _moved = advance(shared, host, machine, HostEvent::Failed { error: detail });
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
        if !matches!(after, HostState::Connected { .. })
            && let Ok(mut credit) = shared.credit.lock()
        {
            credit.disconnect(host);
        }
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
        | Order::Upgrade { .. }
        | Order::Stop => {}
    }
}

/// Whether the pile would hold this kind of order.
///
/// The same four kinds [`keep`] keeps, asked before an order is given away, so
/// that only what would be held is copied: a keystroke, a credit, a command
/// and a screen are gone with the link, and copying a paste to hold what
/// nobody would resend would copy the paste.
fn keeps(order: &Order) -> bool {
    matches!(
        order,
        Order::Subscribe { .. }
            | Order::Unsubscribe { .. }
            | Order::Resize { .. }
            | Order::Focus { .. }
    )
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
                    if carry(&mut channel, order.clone(), shared).await.is_err() {
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
    /// What its server advertised it can decode.
    capabilities: Capabilities,
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
        let greeting = channel.greeting();
        // A socket on this machine is a daemon too, and it may be one this
        // build did not start: its greeting says whether anything is missing,
        // and an offer is the only honest thing to make of that.
        let offer = offer_for(greeting, &Decision::UpToDate);
        let version = greeting.server_version.clone();
        let capabilities = trusted_capabilities(greeting);
        return Ok(Reached {
            channel,
            snapshot,
            version,
            capabilities,
            offer,
        });
    }
    let watching = |stage: Stage| {
        let _reported = advance(shared, host, machine, HostEvent::Reached { stage });
    };
    let connected =
        bootstrap_watched(&transport, &shared.artifacts, &options, deadline, &watching).await?;
    let greeting = connected.channel.greeting();
    let offer = offer_for(greeting, &connected.decision);
    let version = greeting.server_version.clone();
    let capabilities = trusted_capabilities(greeting);
    Ok(Reached {
        channel: connected.channel,
        snapshot: connected.snapshot,
        version,
        capabilities,
        offer,
    })
}

/// The capabilities of a greeting this build may act on.
///
/// A capability bit means what the build that assigned it says it means, and
/// the only thing that identifies a build is its version. Two servers that both
/// say "protocol 1" may have given one bit number two different jobs — an
/// unreleased local build did exactly that — so a server that is not this
/// build's own version is not interpreted at all: its advertisement is dropped
/// and every feature gated on a bit is unavailable to it. That is the
/// conservative answer, and it costs nothing real, because a host of another
/// version is offered an upgrade on that ground alone.
fn trusted_capabilities(greeting: &ServerHello) -> Capabilities {
    if greeting.server_version == bundled().crate_version {
        greeting.capabilities
    } else {
        Capabilities::from_bits(0)
    }
}

/// The upgrade offer a connection puts on the table, if any.
///
/// Four things offer one. A host the probe found another version on is offered
/// it for the version. A host reached over a local socket — where no probe ran
/// — is offered it when its greeting names another version. A host of this
/// build's version whose server is nevertheless missing capabilities is offered
/// it for the gap. The installed server is always read from what the greeting
/// said, never assumed to equal what the build carries.
fn offer_for(greeting: &ServerHello, decision: &Decision) -> Option<UpgradeOffer> {
    // What the greeting said the host runs, which is never assumed to be what
    // this build carries.
    let what_the_host_said = InstalledServer {
        crate_version: greeting.server_version.clone(),
        protocol_version: greeting.protocol_version,
    };
    // The probe's own answer is the one to offer when it found a version this
    // build does not carry.
    if let Decision::UpgradeAvailable {
        installed: found,
        bundled: carried,
    } = decision
    {
        return Some(UpgradeOffer {
            installed: found.clone(),
            bundled: carried.clone(),
            reason: UpgradeReason::Version,
        });
    }
    let carried = bundled();
    if what_the_host_said.crate_version != carried.crate_version {
        // A local socket, or a probe the greeting disagreed with: the version
        // alone is reason enough, and it is the honest one.
        return Some(UpgradeOffer {
            installed: what_the_host_said,
            bundled: carried,
            reason: UpgradeReason::Version,
        });
    }
    if trusted_capabilities(greeting).missing_features().bits() != 0 {
        return Some(UpgradeOffer {
            installed: what_the_host_said,
            bundled: carried,
            reason: UpgradeReason::Capabilities,
        });
    }
    None
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
        capabilities,
        offer,
    } = reached;
    // The snapshot a connection begins with is taken inside the launch, before
    // there is a loop to hear it in, so it is announced from here: what is
    // above the manager is told about a host's model the same way whether the
    // model arrived with the connection or after it.
    told_the_model(host, shared, &snapshot);
    let mut given_up = Vec::new();
    if let Ok(mut model) = shared.model.lock() {
        if let Some(view) = model.host_mut(host) {
            // What the host says replaces what it said before, and
            // whatever is still in flight goes back on top: a command
            // whose answer was lost with the link is still this client's
            // to show, and its rollback must be the model that came back
            // rather than the one from before the drop.
            // The snapshot first, then what no answer can settle any
            // more: this connection is not the one the announcement was
            // owed on, and nothing else takes such a command out.
            given_up = view.settle(snapshot);
            given_up.extend(view.forget_answered());
            view.capabilities = capabilities;
            replay(view);
        } else {
            let mut view = HostView::of(snapshot);
            view.capabilities = capabilities;
            let _first = model.insert(host.clone(), view);
        }
    }
    abandoned(host, &given_up);
    let taken = advance(
        shared,
        host,
        machine,
        HostEvent::Connected {
            server_version: version,
            capabilities,
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
            Turn::Ordered(Some(Order::Upgrade { force })) => {
                // The link is talking to the daemon being replaced, so it goes;
                // the outer loop runs the replacement and connects again. The
                // machine says work is under way, so the window shows an
                // upgrade rather than a host that merely vanished.
                channel.close();
                let _asked = advance(shared, host, machine, HostEvent::UpgradeAsked);
                return Ended::Upgrade { force };
            }
            Turn::Ordered(Some(order)) => {
                let holdable = keeps(&order).then(|| order.clone());
                if carry(&mut channel, order, shared).await.is_err() {
                    // Held for the next connection, under the same rule as an
                    // order that arrived while there was none: what the link
                    // died holding was taken from the application, which was
                    // told so, and dropping it here would lose a subscription
                    // and leave a pane blank for ever.
                    if let Some(held) = holdable {
                        keep(kept, held);
                    }
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
        if !carried(host, shared, &received) {
            return false;
        }
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
fn carried(
    host: &HostId,
    shared: &Arc<Shared>,
    received: &crate::transport::channel::Received,
) -> bool {
    let Ok(mut model) = shared.model.lock() else {
        return false;
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
        return true;
    };
    let Some(receipt) = u32::try_from(received.payload.len())
        .ok()
        .and_then(|bytes| shared.credit.lock().ok()?.receipt(host, pane, bytes))
    else {
        return false;
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
    true
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
async fn carry(
    channel: &mut RemoteChannel,
    order: Order,
    shared: &Shared,
) -> Result<(), ChannelError> {
    let mut granted = None;
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
        Order::Credit { receipt } => {
            let Some(grant) = shared
                .credit
                .lock()
                .ok()
                .and_then(|credit| credit.claim(&receipt))
            else {
                return Ok(());
            };
            let message = ToServer::Credit {
                channel: grant.channel,
                bytes: grant.bytes,
            };
            granted = Some(grant);
            message
        }
        Order::Command { id, command } => ToServer::Command {
            command_id: id,
            payload: encode_session_command(&command).map_err(ChannelError::Message)?,
        },
        // The first three are the loop's own business; the last says the
        // person is looking at no pane at all, which the host is not told
        // because there is nothing for it to prefer.
        Order::Reconnect | Order::Upgrade { .. } | Order::Stop | Order::Focus { pane: None } => {
            return Ok(());
        }
    };
    write(channel, &message).await?;
    if let Some(grant) = granted {
        let _recorded = shared.with(&grant.host, |view| {
            if let Some(subscription) = view.subscription_mut(grant.pane) {
                subscription.grant(u64::from(grant.bytes));
            }
        });
    }
    Ok(())
}
