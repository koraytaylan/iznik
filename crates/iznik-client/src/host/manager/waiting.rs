//! What a host's task does while it has no link: how long it waits, and
//! which of the orders that arrive meanwhile it keeps for the next one.

use std::sync::Mutex;
use std::time::Instant;

use iznik_protocol::identity::PaneId;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::host::manager::Order;
use crate::host::state::{HostState, HostStateMachine};

/// How long a host waits before it is tried again.
pub(super) enum Waiting {
    /// It is not waiting.
    Not,
    /// Until this moment.
    Until(Instant),
    /// Until somebody asks: it failed in a way trying again cannot mend.
    Asked,
}

/// Whether, and until when, a host is waiting to be tried again.
pub(super) fn waiting(machine: &Mutex<HostStateMachine>) -> Waiting {
    let Ok(held) = machine.lock() else {
        return Waiting::Not;
    };
    match held.state() {
        HostState::Reconnecting { retry_at, .. }
        | HostState::Failed {
            retry_at: Some(retry_at),
            ..
        } => Waiting::Until(*retry_at),
        HostState::Failed { retry_at: None, .. } => Waiting::Asked,
        _running => Waiting::Not,
    }
}

/// Waits as long as `wait` says, taking orders meanwhile.
///
/// Answers `false` when the host was told to stop. Orders that need a channel
/// are dropped: there is none, and a keystroke held for a minute and then
/// delivered is worse than one that went nowhere.
pub(super) async fn hold_until(
    wait: &Waiting,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
) -> bool {
    loop {
        let waiting = async {
            match wait {
                Waiting::Until(moment) => tokio::time::sleep_until((*moment).into()).await,
                Waiting::Not => {}
                Waiting::Asked => core::future::pending::<()>().await,
            }
        };
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
pub(super) fn keep(kept: &mut Vec<Order>, order: Order) {
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
pub(super) fn keeps(order: &Order) -> bool {
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
