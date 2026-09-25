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

/// Why a wait ended with the host still held.
pub(super) enum Woken {
    /// It is time to try again, or somebody asked for it now.
    Due,
    /// Somebody asked for its server to be replaced, which is how it is to
    /// be tried again.
    ///
    /// An upgrade asked for while there is no link is not an order to drop:
    /// a forced one exists for exactly the daemon this client can no longer
    /// connect to at all, and dropping it while `upgrade` answered that it was
    /// accepted would leave a host that is never upgraded and never says why.
    Upgrade {
        /// Whether to replace it even though it holds panes.
        force: bool,
        /// Keep the sessions, when the server can adopt them.
        keep_sessions: bool,
    },
}

/// Waits as long as `wait` says, taking orders meanwhile.
///
/// Answers `None` when the host was told to stop. Orders that need a channel
/// are dropped: there is none, and a keystroke held for a minute and then
/// delivered is worse than one that went nowhere — which `dropping` is told,
/// with the pane and how many bytes, so the application hears of it.
pub(super) async fn hold_until(
    wait: &Waiting,
    orders: &mut UnboundedReceiver<Order>,
    kept: &mut Vec<Order>,
    dropping: &(dyn Fn(PaneId, usize) + Sync),
) -> Option<Woken> {
    loop {
        let waiting = async {
            match wait {
                Waiting::Until(moment) => tokio::time::sleep_until((*moment).into()).await,
                Waiting::Not => {}
                Waiting::Asked => core::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = waiting => return Some(Woken::Due),
            order = orders.recv() => match order {
                None | Some(Order::Stop) => return None,
                // Somebody asked for it now, so the wait is over.
                Some(Order::Reconnect) => return Some(Woken::Due),
                Some(Order::Upgrade {
                    force,
                    keep_sessions,
                }) => {
                    return Some(Woken::Upgrade {
                        force,
                        keep_sessions,
                    });
                }
                Some(Order::Input { pane, bytes, .. }) => dropping(pane, bytes.len()),
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
        Order::Subscribe { pane } | Order::Resume { pane, .. } => {
            kept.retain(|held| !about(held, pane));
            kept.push(order);
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
        // The last three end a wait rather than being held through one.
        | Order::Reconnect
        | Order::Upgrade { .. }
        | Order::Stop => {}
    }
}

/// A held order as it is to be carried to a daemon other than the one it was
/// given against.
///
/// A resume names a byte of one daemon's pane, and pane numbers begin again
/// with every daemon: carried to another run it would splice whatever pane
/// now has that number onto the old one's screen. It is asked for afresh
/// instead, which the host answers with a screen. Every other order is about
/// the pane as it now is, and goes as it was given.
pub(super) fn afresh(order: Order) -> Order {
    match order {
        Order::Resume { pane, .. } => Order::Subscribe { pane },
        other => other,
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
            | Order::Resume { .. }
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
        Order::Subscribe { pane: named }
        | Order::Resume { pane: named, .. }
        | Order::Unsubscribe { pane: named } => *named == pane,
        _otherwise => false,
    }
}
