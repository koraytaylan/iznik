//! One connected link's loop: what arrived and what was ordered, until the
//! link goes.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::host::identity::HostId;
use crate::host::manager::hearing::heard;
use crate::host::manager::waiting::{keep, keeps};
use crate::host::manager::{ORDERS_PER_TURN, Order, Shared};
use crate::host::state::{HostEvent, HostStateMachine};
use crate::transport::channel::{ChannelError, Received, RemoteChannel};

use super::{
    Ended, LinkRecord, Turn, admitted, advance, carry, carry_credit, credit_failed, current, dead,
    next_turn,
};

/// The pieces of one host task that a turn of its link reads and changes.
struct LinkTurn<'turn> {
    /// The host this link belongs to.
    host: &'turn HostId,
    /// The manager this host shares with the others.
    shared: &'turn Arc<Shared>,
    /// The host's own state machine.
    machine: &'turn Mutex<HostStateMachine>,
    /// Orders still waiting to be carried.
    orders: &'turn mut UnboundedReceiver<Order>,
    /// Orders held for the next connection.
    kept: &'turn mut Vec<Order>,
    /// What this link already carries.
    record: &'turn mut LinkRecord,
    /// An order taken while gathering credit, served on the next turn.
    pending: &'turn mut Option<Order>,
}

/// The loop one host task runs once a link is up, with what it carried kept
/// in `record`.
pub(super) async fn serve_link(
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
    let mut work = LinkTurn {
        host,
        shared,
        machine,
        orders,
        kept,
        record,
        pending: &mut pending,
    };
    loop {
        let now = Instant::now();
        let soon = now
            .checked_add(work.shared.options.expire_interval)
            .unwrap_or(now);
        let turn = match work.pending.take() {
            Some(order) => Turn::Ordered(Some(order)),
            None => next_turn(&mut channel, work.orders, soon, carried < ORDERS_PER_TURN).await,
        };
        carried = if matches!(turn, Turn::Ordered(_)) {
            carried.saturating_add(1)
        } else {
            0
        };
        match turn {
            Turn::Arrived(arrived) => {
                if let Some(ended) =
                    arrived_turn(work.host, work.shared, work.machine, &mut channel, arrived).await
                {
                    return ended;
                }
            }
            Turn::Ordered(asked) => {
                channel = match ordered_turn(&mut work, channel, asked).await {
                    Ok(open) => open,
                    Err(ended) => return ended,
                };
            }
        }
    }
}

/// What the link said. A deadline is not a failure: the loop goes round so
/// that orders are still taken.
async fn arrived_turn(
    host: &HostId,
    shared: &Arc<Shared>,
    machine: &Mutex<HostStateMachine>,
    channel: &mut RemoteChannel,
    arrived: Result<Received, ChannelError>,
) -> Option<Ended> {
    match arrived {
        Ok(received) => {
            if let Err(detail) = heard(host, shared, channel, received).await {
                let _dead = advance(shared, host, machine, dead(&detail));
                return Some(Ended::Gone);
            }
            None
        }
        Err(ChannelError::Deadline { .. }) => None,
        Err(error) => {
            let _dead = advance(shared, host, machine, dead(&error.to_string()));
            Some(Ended::Gone)
        }
    }
}

/// What the application asked for.
///
/// # Errors
///
/// The link ended on this turn. `Ok` is the link, still open. A reconnection
/// or an upgrade has already closed it.
async fn ordered_turn(
    work: &mut LinkTurn<'_>,
    mut channel: RemoteChannel,
    asked: Option<Order>,
) -> Result<RemoteChannel, Ended> {
    match asked {
        None | Some(Order::Stop) => {
            let _torn = advance(work.shared, work.host, work.machine, HostEvent::Removed);
            Err(Ended::Stopped)
        }
        Some(Order::Reconnect) => {
            channel.close();
            let _dead = advance(
                work.shared,
                work.host,
                work.machine,
                dead("a reconnection was asked for"),
            );
            // Asked for, so the backoff is not waited out.
            let _now = advance(work.shared, work.host, work.machine, HostEvent::RetryDue);
            Err(Ended::Gone)
        }
        Some(Order::Upgrade {
            force,
            keep_sessions,
        }) => {
            // The link is talking to the daemon being replaced, so it goes;
            // the outer loop runs the replacement and connects again. The
            // machine says work is under way, so the window shows an upgrade
            // rather than a host that merely vanished.
            channel.close();
            let _asked = advance(
                work.shared,
                work.host,
                work.machine,
                HostEvent::UpgradeAsked,
            );
            Err(Ended::Upgrade {
                force,
                keep_sessions,
            })
        }
        Some(Order::Credit { receipt }) => {
            let (carrying, after) =
                carry_credit(&mut channel, &receipt, work.orders, work.shared).await;
            if carrying.is_err() {
                return Err(credit_failed(
                    work.host,
                    work.shared,
                    work.machine,
                    work.kept,
                    after,
                ));
            }
            *work.pending = after;
            Ok(channel)
        }
        Some(order) => match deliver_order(work, &mut channel, order).await {
            Some(ended) => Err(ended),
            None => Ok(channel),
        },
    }
}

/// Send one ordinary order. `None` means the link is still up.
///
/// An order the link died holding is kept for the next connection. Dropping
/// it here would lose a subscription and leave a pane blank for ever.
async fn deliver_order(
    work: &mut LinkTurn<'_>,
    channel: &mut RemoteChannel,
    order: Order,
) -> Option<Ended> {
    if !admitted(work.host, work.shared, &order, work.record) {
        return None;
    }
    let order = current(work.host, work.shared, order);
    let holdable = keeps(&order).then(|| order.clone());
    let carrying = carry(work.host, work.shared, channel, order).await;
    if let Err(ChannelError::Message(refusal)) = &carrying {
        // Refused here, before a byte of it was written: the order fails and
        // the link, which never saw it, carries on.
        tracing::warn!(host = ?work.host.0, %refusal, "an order could not be encoded");
        return None;
    }
    if carrying.is_err() {
        if let Some(held) = holdable {
            keep(work.kept, held);
        }
        let _dead = advance(
            work.shared,
            work.host,
            work.machine,
            dead("the link would not take it"),
        );
        return Some(Ended::Gone);
    }
    None
}
