//! What one host's own task does, from its first bootstrap to its last.
//!
//! Everything here runs on the manager's runtime, one instance per host, and
//! touches the shared model only under its lock and never across anything that
//! reaches a network. What it is for is in [`super`]: isolation, and a resume
//! that carries a pane's bytes across a dropped link.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik_protocol::command::encode_session_command;
use iznik_protocol::identity::{CommandId, PaneId, Sequence};
use iznik_protocol::message::{CHANNEL_CONTROL, MAXIMUM_INPUT_LENGTH, ToServer, encode_to_server};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::bootstrap::launch::{Cause, UpgradeError};
use crate::bootstrap::{Replacement, upgrade};
use crate::commands::{abandoned, expire, replay};
use crate::host::identity::HostId;
use crate::host::manager::credit::{CreditBatch, CreditReceipt};
use crate::host::manager::hearing::{heard, told_the_model};
use crate::host::manager::reach::{Reached, reach};
use crate::host::manager::unanswered::{Unanswered, still_asked};
use crate::host::manager::waiting::{Waiting, Woken, afresh, hold_until, keep, keeps, waiting};
use crate::host::manager::{ManagerEvent, ORDERS_PER_TURN, Order, Shared, bootstrapping, seeded};
use crate::host::state::{Action, HostEvent, HostState, HostStateMachine};
use crate::model::HostView;
use crate::reduce::Notification;
use crate::transport::Transport;
use crate::transport::channel::{ChannelError, RemoteChannel};

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
        /// Keep the sessions, when the server can adopt them.
        keep_sessions: bool,
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
    // pane nobody subscribed to is a pane that stays blank. So keystrokes
    // given while there was no link — waiting out a backoff, or queued behind
    // a bootstrap that took minutes — are dropped, and the application is told
    // with `InputDropped`; everything else of the kinds `keep` names is held.
    let mut kept: Vec<Order> = Vec::new();
    let mut unanswered = Unanswered::new();
    loop {
        let Some(link) = connect(
            &host,
            &shared,
            &machine,
            &mut orders,
            &mut kept,
            &mut unanswered,
        )
        .await
        else {
            // It was told to stop while it had no link, and whoever is
            // watching is told so rather than left with a host that simply
            // stopped saying anything.
            shared.publish(ManagerEvent::Removed { host });
            return;
        };
        match pump(
            &host,
            &shared,
            &machine,
            &mut orders,
            &mut kept,
            link,
            &mut unanswered,
        )
        .await
        {
            Ended::Stopped => return,
            Ended::Gone => {}
            Ended::Upgrade {
                force,
                keep_sessions,
            } => {
                let _replaced = replace(&host, &shared, &machine, force, keep_sessions).await;
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
    keep_sessions: bool,
) -> bool {
    let transport = Transport::for_alias(
        &host.0,
        &shared.options.runtime_paths,
        shared.options.ssh.clone(),
    );
    // A daemon still running another build over this build's binary is
    // replaced even though the probe finds nothing to do: that is what its
    // offer was for.
    // And the run this client last reached is what the upgrade must end: a
    // forced one cannot ask the daemon which run it is before stopping it.
    let (superseded, running) = shared
        .with(host, |view| (view.superseded.is_some(), view.instance))
        .unwrap_or((false, None));
    let replaced = upgrade(
        &transport,
        &shared.artifacts,
        &bootstrapping(&shared.options),
        Replacement {
            force,
            stale: superseded,
            running,
            keep_sessions,
        },
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
pub(super) fn advance(
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
        shared.publish(ManagerEvent::Moved {
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
/// itself: a failure retrying cannot mend waits for somebody to ask. With the
/// channel comes the moment it was reached — a keystroke given before it had
/// no link to go on, and is not delivered late — and the commands the last
/// link left unanswered that it sends again first.
async fn connect(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
    unanswered: &mut Unanswered,
) -> Option<Link> {
    loop {
        let wait = waiting(machine);
        if !matches!(wait, Waiting::Not) {
            let dropping = |pane: PaneId, bytes: usize| dropped_input(host, shared, pane, bytes);
            let upload_lost = |pane: PaneId| {
                crate::host::manager::upload::failed(
                    host,
                    shared,
                    pane,
                    "the host had no link to receive the file".to_owned(),
                );
            };
            match hold_until(&wait, orders, kept, &dropping, &upload_lost).await {
                None => {
                    let _torn = advance(shared, host, machine, HostEvent::Removed);
                    return None;
                }
                Some(Woken::Due) => {
                    let _tried = advance(shared, host, machine, HostEvent::RetryDue);
                }
                // Replaced first, then reached: the replacement is what may
                // make the host reachable at all.
                Some(Woken::Upgrade {
                    force,
                    keep_sessions,
                }) => {
                    let _asked = advance(shared, host, machine, HostEvent::UpgradeAsked);
                    if !replace(host, shared, machine, force, keep_sessions).await {
                        continue;
                    }
                }
            }
        }
        match reach(host, shared, machine).await {
            Ok(reached) => {
                let linked = Instant::now();
                let mut channel = accept(host, shared, machine, reached).await;
                let carried = unanswered.link_began(host, shared, &mut channel).await;
                // Everything asked for while there was nowhere to send it.
                // What the link would not take stays held: the next
                // connection is the one that will carry it, and a write that
                // failed is a link that has just died. A resume held from
                // before a restart is asked for afresh, as `resume` asks for
                // every other pane.
                let standing: Vec<Order> = std::mem::take(kept)
                    .into_iter()
                    .map(|order| current(host, shared, order))
                    .collect();
                let mut sent = 0_usize;
                for order in &standing {
                    match carry(host, shared, &mut channel, order.clone()).await {
                        // One order this client could not even encode is that
                        // order's failure, and the link is fine.
                        Ok(()) | Err(ChannelError::Message(_)) => {}
                        Err(_link) => break,
                    }
                    sent = sent.saturating_add(1);
                }
                kept.extend(standing.into_iter().skip(sent));
                let record = LinkRecord { linked, carried };
                return Some(Link { channel, record });
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

/// An order as it is to be carried to the daemon the host has now.
///
/// A resume names a byte of the run of the daemon that last carried its pane;
/// to any other run it is asked for afresh. Every other order goes as given.
fn current(host: &HostId, shared: &Shared, order: Order) -> Order {
    let Order::Resume {
        instance: Some(from),
        ..
    } = &order
    else {
        return order;
    };
    let restarted = shared
        .with(host, |view| view.instance.is_some_and(|now| now != *from))
        .unwrap_or(false);
    if restarted { afresh(order) } else { order }
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
        superseded,
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
            // A new connection has asked for nothing yet: the launch's own
            // snapshot is what it began with.
            view.snapshot_asked = false;
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
            view.superseded = superseded;
            replay(view);
        } else {
            let mut view = HostView::of(snapshot);
            view.capabilities = capabilities;
            view.instance = instance;
            view.superseded = superseded;
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
        // Said, so that whatever draws a pane this client has let go of can
        // start it again rather than resume it from a byte of a pane that is
        // gone; the panes still subscribed are answered with screens.
        shared.publish(ManagerEvent::Notify(Notification::DaemonRestarted {
            host: host.clone(),
        }));
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

/// Serves a connected host until its link goes or it is told to stop, and
/// then says what became of every command it carried and nobody answered.
async fn pump(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
    link: Link,
    unanswered: &mut Unanswered,
) -> Ended {
    let Link {
        channel,
        mut record,
    } = link;
    let ended = serve_link(host, shared, machine, orders, kept, channel, &mut record).await;
    if !matches!(ended, Ended::Stopped) {
        unanswered.link_ended(host, shared, &record.carried);
    }
    ended
}

/// A link just reached, and what it already carries.
struct Link {
    /// The channel.
    channel: RemoteChannel,
    /// What it carries.
    record: LinkRecord,
}

/// What one link did that outlives it.
struct LinkRecord {
    /// When it was reached: a keystroke given before this had no link.
    linked: Instant,
    /// Every command it carried, whose answers it may not live to hear.
    carried: Vec<CommandId>,
}

/// Tells the application keystrokes were not delivered, because there was
/// no link when they were given.
pub(super) fn dropped_input(host: &HostId, shared: &Shared, pane: PaneId, bytes: usize) {
    shared.publish(ManagerEvent::Notify(Notification::InputDropped {
        host: host.clone(),
        pane,
        bytes,
    }));
}

/// Whether an order may go out on this link, recording what the link must
/// answer for.
///
/// A keystroke given before the link was reached is dropped and said so: a
/// key pressed while a bootstrap ran for a minute and then delivered is worse
/// than one that went nowhere. A command already taken back is not sent. A
/// command that is sent is counted, so a link that goes before its answer
/// can say what became of it.
fn admitted(host: &HostId, shared: &Shared, order: &Order, record: &mut LinkRecord) -> bool {
    match order {
        Order::Input { pane, bytes, given } if *given < record.linked => {
            dropped_input(host, shared, *pane, bytes.len());
            false
        }
        Order::Command { id, .. } => {
            if !still_asked(host, shared, *id) {
                return false;
            }
            // Counted before the write: a link that fails part way through it
            // may still have delivered the whole frame.
            record.carried.push(*id);
            true
        }
        _otherwise => true,
    }
}

/// The loop [`pump`] runs: what arrived and what was ordered, until the link
/// goes, with what it carried kept in `record`.
async fn serve_link(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
    mut channel: RemoteChannel,
    record: &mut LinkRecord,
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
            Turn::Ordered(Some(Order::Upgrade {
                force,
                keep_sessions,
            })) => {
                // The link is talking to the daemon being replaced, so it goes;
                // the outer loop runs the replacement and connects again. The
                // machine says work is under way, so the window shows an
                // upgrade rather than a host that merely vanished.
                channel.close();
                let _asked = advance(shared, host, machine, HostEvent::UpgradeAsked);
                return Ended::Upgrade {
                    force,
                    keep_sessions,
                };
            }
            Turn::Ordered(Some(Order::Credit { receipt })) => {
                let (carrying, after) = carry_credit(&mut channel, &receipt, orders, shared).await;
                if carrying.is_err() {
                    return credit_failed(host, shared, machine, kept, after);
                }
                pending = after;
            }
            Turn::Ordered(Some(order)) => {
                if !admitted(host, shared, &order, record) {
                    continue;
                }
                let order = current(host, shared, order);
                let holdable = keeps(&order).then(|| order.clone());
                let carrying = carry(host, shared, &mut channel, order).await;
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

/// How a link ends whose credit would not go, with `after` — the order taken
/// off the queue from behind the credit — not lost with it.
///
/// A stop is still a stop, and an upgrade still an upgrade; keystrokes are
/// dropped and said so, as any given with no link to carry them; the rest is
/// held for the next connection under [`keep`]'s rule.
fn credit_failed(
    host: &HostId,
    shared: &Shared,
    machine: &Mutex<HostStateMachine>,
    kept: &mut Vec<Order>,
    after: Option<Order>,
) -> Ended {
    match after {
        Some(Order::Stop) => {
            let _torn = advance(shared, host, machine, HostEvent::Removed);
            return Ended::Stopped;
        }
        Some(Order::Upgrade {
            force,
            keep_sessions,
        }) => {
            let _asked = advance(shared, host, machine, HostEvent::UpgradeAsked);
            return Ended::Upgrade {
                force,
                keep_sessions,
            };
        }
        Some(Order::Input { pane, bytes, .. }) => dropped_input(host, shared, pane, bytes.len()),
        Some(Order::Upload { pane, .. }) => crate::host::manager::upload::failed(
            host,
            shared,
            pane,
            "the link failed before the file was sent".to_owned(),
        ),
        Some(held) => keep(kept, held),
        None => {}
    }
    let _dead = advance(shared, host, machine, dead("the link would not take it"));
    Ended::Gone
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
        shared.publish(ManagerEvent::Notify(notification));
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
    host: &HostId,
    shared: &Arc<Shared>,
    channel: &mut RemoteChannel,
    order: Order,
) -> Result<(), ChannelError> {
    let message = match order {
        Order::Subscribe { pane } => ToServer::Subscribe { pane },
        Order::Resume { pane, from, .. } => ToServer::Resume {
            pane,
            from_sequence: from,
        },
        Order::Unsubscribe { pane } => ToServer::Unsubscribe { pane },
        Order::Input { pane, bytes, .. } => return carry_input(channel, pane, bytes).await,
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
        Order::Upload {
            pane,
            name,
            path,
            offset,
        } => {
            return crate::host::manager::upload::carry(
                host, shared, channel, pane, name, path, offset,
            )
            .await;
        }
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
