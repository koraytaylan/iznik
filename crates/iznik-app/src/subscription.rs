//! Which panes this window has asked a host to carry, and what the host said.
//!
//! Queuing a `Subscribe` is not being subscribed. The host may refuse it — no
//! channel is free, or the pane is not one it holds — and it may stop
//! carrying a pane on its own, with a `PaneDetached`. A window that counted
//! the order as the answer would show those panes blank for good. So each
//! pane's standing here moves only on what the engine says back: a screen is
//! the answer to a subscribe, a detach ends the carrying, and a refusal puts
//! the asking off for a while and asks again, waiting longer each time.
//!
//! The host's refusal names no pane, so a refusal is taken to be about every
//! pane still waiting for its answer on that host. One that was in fact
//! carried says so with its screen, which settles it whatever was assumed.
//! When channels run out, the panes that are not shown let theirs go, so the
//! ones that are shown come first.
//!
//! What is here is a value, with no engine in it: the window applies what it
//! decides, and a case can establish what it decides without a host.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use iznik_client::host::identity::HostId;
use iznik_protocol::message::ErrorCode;

use crate::bridge::EngineBridge;
use crate::vt::PaneKey;

/// How long the first refusal puts a pane's asking off.
pub const FIRST_RETRY: Duration = Duration::from_millis(250);
/// The longest a refusal puts it off, however many came before.
pub const LONGEST_RETRY: Duration = Duration::from_secs(8);
/// How much longer each refusal in a row waits than the one before.
const RETRY_GROWTH: u32 = 2;

/// Where one pane's carrying stands, as far as the host has said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Not asked for, or no longer carried.
    Idle,
    /// Asked for, and no answer yet.
    Asked {
        /// How long the next refusal waits.
        wait: Duration,
    },
    /// The host answered with a screen: it is carrying the pane.
    Carried,
    /// The host refused; ask again once `after` has passed.
    Refused {
        /// When to ask again.
        after: Instant,
        /// How long the refusal after that one waits.
        wait: Duration,
    },
}

/// What the window has to ask a host for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    /// Start carrying this pane.
    Subscribe(PaneKey),
    /// Stop carrying it.
    Unsubscribe(PaneKey),
}

/// Every pane the window holds, and where its carrying stands.
#[derive(Clone, Debug, Default)]
pub struct Subscriptions {
    /// Each held pane's standing.
    panes: BTreeMap<PaneKey, Standing>,
}

impl Subscriptions {
    /// Nothing held and nothing asked for.
    #[must_use]
    pub fn new() -> Subscriptions {
        Subscriptions::default()
    }

    /// Where one pane stands; a pane not held is idle.
    #[must_use]
    pub fn standing(&self, key: &PaneKey) -> Standing {
        self.panes.get(key).copied().unwrap_or(Standing::Idle)
    }

    /// Forget a pane the model no longer holds.
    pub fn forget(&mut self, key: &PaneKey) {
        let _gone = self.panes.remove(key);
    }

    /// A screen arrived for a pane: whatever was asked for is being carried.
    pub fn screen(&mut self, key: &PaneKey) {
        if let Some(standing) = self.panes.get_mut(key) {
            *standing = Standing::Carried;
        }
    }

    /// The host stopped carrying a pane.
    ///
    /// A pane still waiting for its answer keeps waiting: the detach is the
    /// end of the stream an earlier unsubscribe asked to end, and the one now
    /// asked for has not been announced yet.
    pub fn detached(&mut self, key: &PaneKey) {
        if let Some(standing) = self.panes.get_mut(key)
            && *standing == Standing::Carried
        {
            *standing = Standing::Idle;
        }
    }

    /// A host refused something. A refusal about subscribing puts off every
    /// pane still waiting on that host, and running out of channels also
    /// lets the panes that are not shown give theirs back.
    pub fn refused(
        &mut self,
        host: &HostId,
        code: ErrorCode,
        shown: &BTreeSet<PaneKey>,
        now: Instant,
    ) -> Vec<Order> {
        if !matches!(code, ErrorCode::ChannelsExhausted | ErrorCode::UnknownPane) {
            return Vec::new();
        }
        let mut orders = Vec::new();
        for (key, standing) in &mut self.panes {
            if key.host != *host {
                continue;
            }
            match *standing {
                Standing::Asked { wait } => *standing = put_off(wait, now),
                Standing::Carried
                    if code == ErrorCode::ChannelsExhausted && !shown.contains(key) =>
                {
                    *standing = Standing::Idle;
                    orders.push(Order::Unsubscribe(key.clone()));
                }
                Standing::Idle | Standing::Carried | Standing::Refused { .. } => {}
            }
        }
        orders
    }

    /// An order the engine would not take: ask again after a wait, as for a
    /// refusal, rather than on every turn.
    pub fn not_sent(&mut self, order: &Order, now: Instant) {
        let Order::Subscribe(key) = order else {
            return;
        };
        if let Some(standing) = self.panes.get_mut(key)
            && let Standing::Asked { wait } = *standing
        {
            *standing = put_off(wait, now);
        }
    }

    /// What to ask for so that exactly the shown panes are carried: every
    /// shown pane that is idle, or whose wait after a refusal is over, is
    /// subscribed, and every other pane that is carried or asked for is let go.
    pub fn plan(&mut self, shown: &BTreeSet<PaneKey>, now: Instant) -> Vec<Order> {
        for key in shown {
            let _held = self.panes.entry(key.clone()).or_insert(Standing::Idle);
        }
        let mut orders = Vec::new();
        for (key, standing) in &mut self.panes {
            let visible = shown.contains(key);
            match *standing {
                Standing::Idle if visible => {
                    *standing = Standing::Asked { wait: FIRST_RETRY };
                    orders.push(Order::Subscribe(key.clone()));
                }
                Standing::Refused { after, wait } if visible && now >= after => {
                    *standing = Standing::Asked { wait };
                    orders.push(Order::Subscribe(key.clone()));
                }
                Standing::Refused { .. } if !visible => *standing = Standing::Idle,
                Standing::Asked { .. } | Standing::Carried if !visible => {
                    *standing = Standing::Idle;
                    orders.push(Order::Unsubscribe(key.clone()));
                }
                Standing::Idle
                | Standing::Asked { .. }
                | Standing::Carried
                | Standing::Refused { .. } => {}
            }
        }
        orders
    }
}

/// A refused pane's standing: ask again after `wait`, and wait longer the
/// next time, up to [`LONGEST_RETRY`].
fn put_off(wait: Duration, now: Instant) -> Standing {
    Standing::Refused {
        after: now.checked_add(wait).unwrap_or(now),
        wait: wait.saturating_mul(RETRY_GROWTH).min(LONGEST_RETRY),
    }
}

/// Hands each order to the engine. An order it would not take puts the pane
/// off as a refusal does, and is answered with the host and what was said.
pub(crate) fn send(
    subscriptions: &mut Subscriptions,
    bridge: &EngineBridge,
    orders: &[Order],
    now: Instant,
) -> Vec<(HostId, String)> {
    let mut failures = Vec::new();
    for order in orders {
        let (key, result) = match order {
            Order::Subscribe(key) => (key, bridge.subscribe(&key.host.0, key.pane)),
            Order::Unsubscribe(key) => (key, bridge.unsubscribe(&key.host.0, key.pane)),
        };
        if let Err(error) = result {
            subscriptions.not_sent(order, now);
            failures.push((key.host.clone(), error.to_string()));
        }
    }
    failures
}
