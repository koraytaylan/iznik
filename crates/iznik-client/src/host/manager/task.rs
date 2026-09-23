//! What one host's own task does, from its first bootstrap to its last.
//!
//! Everything here runs on the manager's runtime, one instance per host, and
//! touches the shared model only under its lock and never across anything that
//! reaches a network. What it is for is in [`super`]: isolation, and a resume
//! that carries a pane's bytes across a dropped link.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::encode_session_command;
use iznik_protocol::identity::{DaemonInstance, PaneId, Sequence};
use iznik_protocol::message::{CHANNEL_CONTROL, MAXIMUM_INPUT_LENGTH, ToServer, encode_to_server};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::bootstrap::launch::{
    BootstrapError, Cause, Decision, Stage, UpgradeError, bundled, expiry, launch,
};
use crate::bootstrap::probe::InstalledServer;
use crate::bootstrap::{bootstrap_watched, upgrade};
use crate::commands::{abandoned, expire, replay};
use crate::host::identity::HostId;
use crate::host::manager::credit::{CreditBatch, CreditReceipt};
use crate::host::manager::hearing::{heard, told_the_model};
use crate::host::manager::waiting::{Waiting, Woken, hold_until, keep, keeps, waiting};
use crate::host::manager::{ManagerEvent, ORDERS_PER_TURN, Order, Shared, bootstrapping, seeded};
use crate::host::state::{
    Action, HostEvent, HostState, HostStateMachine, UpgradeOffer, UpgradeReason,
};
use crate::model::HostView;
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
            // It was told to stop while it had no link, and whoever is
            // watching is told so rather than left with a host that simply
            // stopped saying anything.
            shared.publish(&ManagerEvent::Removed { host });
            return;
        };
        match pump(&host, &shared, &machine, &mut orders, &mut kept, channel).await {
            Ended::Stopped => return,
            Ended::Gone => {}
            Ended::Upgrade { force } => {
                let _replaced = replace(&host, &shared, &machine, force).await;
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
/// that did not happen must not look like one that did — and answered, so a
/// caller can wait out what the refusal scheduled before trying again.
async fn replace(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    force: bool,
) -> bool {
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
        Err(refusal) => {
            let (cause, stage) = match &refusal {
                UpgradeError::Bootstrap(source) => (source.cause, Some(source.stage)),
                UpgradeError::LivePanes { .. } => (Cause::Transient, None),
            };
            Some((format!("upgrading {host}: {refusal}"), cause, stage))
        }
    };
    // The next `connect` is what reaches the new server; say the failure now
    // and let the reconnect that follows say the rest.
    let Some((detail, cause, stage)) = error else {
        return true;
    };
    let _moved = advance(
        shared,
        host,
        machine,
        HostEvent::Failed {
            error: detail,
            cause,
            stage,
        },
    );
    false
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
        // Debug, not Display: a state carries what the server said it is,
        // and a server that names itself with a newline or an escape must
        // not be able to write lines of its own into this log.
        tracing::info!(
            host = ?host.0,
            from = ?before.to_string(),
            to = ?after.to_string(),
            "a host moved"
        );
        shared.publish(&ManagerEvent::Moved {
            host: host.clone(),
            state: after,
        });
    }
    taken
}

/// Gets a channel to the host, waiting out whatever backoff the machine holds
/// and trying again for as long as it says to.
///
/// Answers `None` when the host was told to stop, and never gives up by
/// itself: a failure retrying cannot mend waits for somebody to ask.
async fn connect(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
) -> Option<RemoteChannel> {
    loop {
        let wait = waiting(machine);
        if !matches!(wait, Waiting::Not) {
            match hold_until(&wait, orders, kept).await {
                None => {
                    let _torn = advance(shared, host, machine, HostEvent::Removed);
                    return None;
                }
                Some(Woken::Due) => {
                    let _tried = advance(shared, host, machine, HostEvent::RetryDue);
                }
                // Replaced first, then reached: the replacement is what may
                // make the host reachable at all.
                Some(Woken::Upgrade { force }) => {
                    let _asked = advance(shared, host, machine, HostEvent::UpgradeAsked);
                    if !replace(host, shared, machine, force).await {
                        continue;
                    }
                }
            }
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
                    match carry(&mut channel, order.clone(), shared).await {
                        // One order this client could not even encode is that
                        // order's failure, and the link is fine.
                        Ok(()) | Err(ChannelError::Message(_)) => {}
                        Err(_link) => break,
                    }
                    sent = sent.saturating_add(1);
                }
                kept.extend(standing.into_iter().skip(sent));
                return Some(channel);
            }
            // The machine decides what comes next — a wait, or for a failure
            // trying again cannot mend, a wait for somebody to ask — and the
            // top of this loop waits it out either way.
            Err(error) => {
                let _held = advance(
                    shared,
                    host,
                    machine,
                    HostEvent::Failed {
                        error: error.to_string(),
                        cause: error.cause,
                        stage: Some(error.stage),
                    },
                );
            }
        }
    }
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
    /// Which run of the daemon answered, when it said.
    instance: Option<DaemonInstance>,
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
        let instance = greeting.instance;
        return Ok(Reached {
            channel,
            snapshot,
            version,
            capabilities,
            instance,
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
    let instance = greeting.instance;
    Ok(Reached {
        channel: connected.channel,
        snapshot: connected.snapshot,
        version,
        capabilities,
        instance,
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
        instance,
        offer,
    } = reached;
    // The snapshot a connection begins with is taken inside the launch, before
    // there is a loop to hear it in, so it is announced from here: what is
    // above the manager is told about a host's model the same way whether the
    // model arrived with the connection or after it.
    told_the_model(host, shared, &snapshot);
    let mut given_up = Vec::new();
    // Whether the daemon this reached is another one than the view's state
    // came from: its pane numbers then name other panes, and nothing may be
    // resumed from a byte of a pane that is gone.
    let mut fresh = false;
    if let Ok(mut model) = shared.model.lock() {
        if let Some(view) = model.host_mut(host) {
            fresh = view.reached(instance);
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
            view.instance = instance;
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
    if fresh {
        tracing::info!(
            host = ?host.0,
            "the host's daemon is another than the one this client last reached; \
             every pane is asked for afresh rather than resumed"
        );
    }
    if taken.contains(&Action::Resume) {
        resume(host, shared, &mut channel, fresh).await;
    }
    channel
}

/// Asks the host to carry on every subscribed pane from the byte this client
/// holds, and tells it again which pane the person is looking at.
///
/// This is the whole point of keeping a cursor: a person who closed a laptop
/// sees what arrived while it was shut, rather than a fresh screen. The focus
/// goes with it because it belongs to the connection on the host's side — a
/// new link starts with no pane preferred, and a person who has not moved
/// since would otherwise have their pane share bandwidth with every other
/// until they happened to click.
///
/// Except when the daemon is `fresh` — another run than the one the cursors
/// came from. A pane number there may name a different pane, and resuming it
/// would splice that pane's bytes onto the old one's screen; so every pane the
/// new model still holds is subscribed afresh, which the host answers with a
/// screen, and every pane it does not is let go.
async fn resume(host: &HostId, shared: &Arc<Shared>, channel: &mut RemoteChannel, fresh: bool) {
    let (held, focus): (Vec<(PaneId, Sequence)>, Option<PaneId>) = shared
        .model
        .lock()
        .ok()
        .and_then(|mut model| {
            model.host_mut(host).map(|view| {
                if fresh {
                    let gone: Vec<PaneId> = view
                        .subscriptions
                        .keys()
                        .copied()
                        .filter(|pane| !holds(&view.settled, *pane))
                        .collect();
                    for pane in gone {
                        let _dropped = view.unsubscribe(pane);
                    }
                }
                let held = view
                    .subscriptions
                    .iter()
                    .map(|(pane, held)| (*pane, held.cursor))
                    .collect();
                (held, view.focus)
            })
        })
        .unwrap_or_default();
    for (pane, from_sequence) in held {
        let asked = if fresh {
            ToServer::Subscribe { pane }
        } else {
            ToServer::Resume {
                pane,
                from_sequence,
            }
        };
        let _sent = write(channel, &asked).await;
    }
    if let Some(pane) = focus {
        let _sent = write(channel, &ToServer::Focus { pane }).await;
    }
}

/// Whether a host's model holds a pane.
fn holds(model: &iznik_protocol::model::HostModel, pane: PaneId) -> bool {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .any(|held| held.id == pane)
}

/// Writes one message on the control channel.
///
/// # Errors
///
/// Whatever the channel says, when the link will not take it.
pub(super) async fn write(
    channel: &mut RemoteChannel,
    message: &ToServer,
) -> Result<(), ChannelError> {
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
    // An order taken off the queue while gathering credit, and not credit:
    // the next turn is its.
    let mut pending: Option<Order> = None;
    loop {
        let now = Instant::now();
        let soon = now
            .checked_add(shared.options.expire_interval)
            .unwrap_or(now);
        let turn = match pending.take() {
            Some(order) => Turn::Ordered(Some(order)),
            None => next_turn(&mut channel, orders, soon, carried < ORDERS_PER_TURN).await,
        };
        carried = if matches!(turn, Turn::Ordered(_)) {
            carried.saturating_add(1)
        } else {
            0
        };
        match turn {
            Turn::Arrived(Ok(received)) => {
                if let Err(detail) = heard(host, shared, &mut channel, received).await {
                    let _dead = advance(shared, host, machine, dead(&detail));
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
            Turn::Ordered(Some(Order::Credit { receipt })) => {
                let (carrying, after) = carry_credit(&mut channel, &receipt, orders, shared).await;
                pending = after;
                if carrying.is_err() {
                    let _dead = advance(shared, host, machine, dead("the link would not take it"));
                    return Ended::Gone;
                }
            }
            Turn::Ordered(Some(order)) => {
                let holdable = keeps(&order).then(|| order.clone());
                let carrying = carry(&mut channel, order, shared).await;
                if let Err(ChannelError::Message(refusal)) = &carrying {
                    // Refused here, before a byte of it was written: the order
                    // fails and the link, which never saw it, carries on.
                    tracing::warn!(host = ?host.0, %refusal, "an order could not be encoded");
                    continue;
                }
                if carrying.is_err() {
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

/// Writes keystrokes on the channel, as as many `Input` messages as they need.
///
/// A paste larger than one message can carry would otherwise fail to encode —
/// and a link that died of it would take every pane on the host with it, and
/// the paste too. A pane reads a byte stream, so where it is cut is invisible
/// to the program reading it; what matters is that the pieces go in order,
/// which one writer on one link guarantees.
///
/// # Errors
///
/// Whatever the channel says, when the link will not take a piece.
async fn carry_input(
    channel: &mut RemoteChannel,
    pane: PaneId,
    bytes: Vec<u8>,
) -> Result<(), ChannelError> {
    let most = usize::try_from(MAXIMUM_INPUT_LENGTH)
        .unwrap_or(usize::MAX)
        .max(1);
    if bytes.len() <= most {
        return write(channel, &ToServer::Input { pane, bytes }).await;
    }
    for piece in bytes.chunks(most) {
        write(
            channel,
            &ToServer::Input {
                pane,
                bytes: piece.to_vec(),
            },
        )
        .await?;
    }
    Ok(())
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
    let message = match order {
        Order::Subscribe { pane } => ToServer::Subscribe { pane },
        Order::Unsubscribe { pane } => ToServer::Unsubscribe { pane },
        Order::Input { pane, bytes } => return carry_input(channel, pane, bytes).await,
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
            let mut batch = CreditBatch::default();
            claim_into(&mut batch, &receipt, shared);
            return send_credit(channel, batch, shared).await;
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
    write(channel, &message).await
}

/// Claims `first` and every credit order already waiting behind it, and
/// writes what they add up to: one message per stream rather than one per
/// delivery, which for a pane printing fast is the difference between a
/// credit frame per read and a credit frame per turn.
///
/// Gives back, with how the writing went, the first order behind them that
/// was not credit, for the loop to take next.
async fn carry_credit(
    channel: &mut RemoteChannel,
    first: &CreditReceipt,
    orders: &mut UnboundedReceiver<Order>,
    shared: &Shared,
) -> (Result<(), ChannelError>, Option<Order>) {
    let mut batch = CreditBatch::default();
    claim_into(&mut batch, first, shared);
    let mut after = None;
    while let Ok(order) = orders.try_recv() {
        match order {
            Order::Credit { receipt } => claim_into(&mut batch, &receipt, shared),
            other => {
                after = Some(other);
                break;
            }
        }
    }
    (send_credit(channel, batch, shared).await, after)
}

/// Admits one receipt, once and only while its stream is current, into what
/// this turn will return.
fn claim_into(batch: &mut CreditBatch, receipt: &CreditReceipt, shared: &Shared) {
    let admitted = shared.credit.lock().ok().and_then(|mut credit| {
        let grant = credit.claim(receipt)?;
        credit.returned(&grant);
        Some(grant)
    });
    if let Some(grant) = admitted {
        batch.add(grant);
    }
}

/// Writes what a turn claimed, and records each grant against its pane once
/// the link has taken it.
///
/// # Errors
///
/// Whatever the channel says, when the link will not take one.
async fn send_credit(
    channel: &mut RemoteChannel,
    batch: CreditBatch,
    shared: &Shared,
) -> Result<(), ChannelError> {
    for grant in batch.into_grants() {
        write(
            channel,
            &ToServer::Credit {
                channel: grant.channel,
                bytes: grant.bytes,
            },
        )
        .await?;
        let _recorded = shared.with(&grant.host, |view| {
            if let Some(subscription) = view.subscription_mut(grant.pane) {
                subscription.grant(u64::from(grant.bytes));
            }
        });
    }
    Ok(())
}
