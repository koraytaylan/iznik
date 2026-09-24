//! Which panes this window has asked a host to carry, and what the host said.
//!
//! Queuing a `Subscribe` is not being subscribed. The host may refuse it — no
//! channel is free, or the pane is not one it holds — and it may stop
//! carrying a pane on its own, with a `PaneDetached`. A window that counted
//! the order as the answer would show those panes blank for good. So each
//! pane's standing here moves only on what the engine says back: the host
//! saying it carries the pane is the answer, a detach ends the carrying, and a refusal puts
//! the asking off for a while and asks again, waiting longer each time.
//!
//! The host's refusal names no pane, so a refusal is taken to be about every
//! pane still waiting for its answer on that host. One that was in fact
//! carried says so with its screen, which settles it whatever was assumed.
//! When channels run out, the panes that are not shown let theirs go, so the
//! ones that are shown come first.
//!
//! A pane that stops being shown is not let go at once: the most recently
//! shown hidden panes stay carried, so switching back to a tab finds its
//! panes where they were rather than subscribing afresh and fetching every
//! screen whole again.
//!
//! What is here is a value, with no engine in it: the window applies what it
//! decides, and a case can establish what it decides without a host.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use gpui_kit::Context;

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::ErrorCode;

use crate::bridge::EngineBridge;
use crate::vt::PaneKey;
use crate::window::WindowShell;

/// How long the first refusal puts a pane's asking off.
pub const FIRST_RETRY: Duration = Duration::from_millis(250);
/// The longest a refusal puts it off, however many came before.
pub const LONGEST_RETRY: Duration = Duration::from_secs(8);
/// How much longer each refusal in a row waits than the one before.
const RETRY_GROWTH: u32 = 2;
/// How many panes that are not shown stay carried, the most recently shown
/// first, so going back to a tab finds its panes live instead of asking for
/// their whole screens again. Far below the 255 channels a connection has.
pub const HIDDEN_KEPT: usize = 32;

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
    /// When each pane was last shown, which decides the hidden ones kept.
    last_shown: BTreeMap<PaneKey, Instant>,
    /// Where each pane's emulator will stand once the terminal thread has
    /// taken everything handed to it: the byte after the last screen or
    /// output forwarded there. Absent when unknown, or after a gap.
    next: BTreeMap<PaneKey, Sequence>,
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
        let _forgotten = self.last_shown.remove(key);
        let _position = self.next.remove(key);
    }

    /// Note a screen or output handed to a pane's emulator, so that asking
    /// for the pane again resumes from the byte after it — not from what the
    /// emulator has published so far, which lags what is queued to it.
    /// Output that does not continue from the known position is a gap the
    /// emulator will not take, and leaves the position unknown.
    pub fn forwarded(&mut self, said: &ManagerEvent) {
        match said {
            ManagerEvent::Screen {
                host,
                pane,
                sequence,
                ..
            } => {
                let _replaced = self.next.insert(pane_key(host, *pane), *sequence);
            }
            ManagerEvent::Bytes {
                host,
                pane,
                sequence,
                bytes,
                ..
            } => {
                let key = pane_key(host, *pane);
                let after = u64::try_from(bytes.len())
                    .ok()
                    .and_then(|length| sequence.0.checked_add(length));
                match (self.next.get(&key), after) {
                    (Some(at), Some(after)) if at == sequence => {
                        let _advanced = self.next.insert(key, Sequence(after));
                    }
                    _ => {
                        let _gap = self.next.remove(&key);
                    }
                }
            }
            _ => {}
        }
    }

    /// A pane's emulator failed or found a gap: where it stands is no longer
    /// known, and it is asked for afresh rather than resumed.
    pub fn lost(&mut self, key: &PaneKey) {
        let _unknown = self.next.remove(key);
    }

    /// The byte to resume a pane from: the one after everything forwarded to
    /// its emulator, when that is known.
    #[must_use]
    pub fn resume_from(&self, key: &PaneKey) -> Option<Sequence> {
        self.next.get(key).copied()
    }

    /// The host began carrying a pane — its screen arrived, or it said it is
    /// continuing from where the window's emulator stands.
    pub fn carried(&mut self, key: &PaneKey) {
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

    /// What to ask for so that every shown pane is carried: each shown pane
    /// that is idle, or whose wait after a refusal is over, is subscribed.
    /// Panes no longer shown stay carried, up to [`HIDDEN_KEPT`] of the most
    /// recently shown, and the rest are let go.
    pub fn plan(&mut self, shown: &BTreeSet<PaneKey>, now: Instant) -> Vec<Order> {
        for key in shown {
            let _held = self.panes.entry(key.clone()).or_insert(Standing::Idle);
            let _seen = self.last_shown.insert(key.clone(), now);
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
                Standing::Idle
                | Standing::Asked { .. }
                | Standing::Carried
                | Standing::Refused { .. } => {}
            }
        }
        orders.extend(self.let_go_beyond_kept(shown));
        orders
    }

    /// Let go of the hidden panes past the [`HIDDEN_KEPT`] most recently shown.
    fn let_go_beyond_kept(&mut self, shown: &BTreeSet<PaneKey>) -> Vec<Order> {
        let mut hidden: Vec<(Option<Instant>, PaneKey)> = self
            .panes
            .iter()
            .filter(|(key, standing)| {
                !shown.contains(*key)
                    && matches!(standing, Standing::Asked { .. } | Standing::Carried)
            })
            .map(|(key, _standing)| (self.last_shown.get(key).copied(), key.clone()))
            .collect();
        hidden.sort_by(|left, right| right.cmp(left));
        let mut orders = Vec::new();
        for (_shown_at, key) in hidden.into_iter().skip(HIDDEN_KEPT) {
            if let Some(standing) = self.panes.get_mut(&key) {
                *standing = Standing::Idle;
            }
            orders.push(Order::Unsubscribe(key));
        }
        orders
    }
}

/// A pane's key from the host and pane an event names.
fn pane_key(host: &HostId, pane: PaneId) -> PaneKey {
    PaneKey {
        host: host.clone(),
        pane,
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
///
/// A pane whose emulator will stand at a known byte — shown before, then let
/// go — is resumed from there rather than subscribed afresh, so showing it
/// again needs no screen when the host still holds what it missed.
pub(crate) fn send(
    subscriptions: &mut Subscriptions,
    bridge: &EngineBridge,
    orders: &[Order],
    now: Instant,
) -> Vec<(HostId, String)> {
    let mut failures = Vec::new();
    for order in orders {
        let (key, result) = match order {
            Order::Subscribe(key) => match subscriptions.resume_from(key) {
                Some(from) => (key, bridge.resume(&key.host.0, key.pane, from)),
                None => (key, bridge.subscribe(&key.host.0, key.pane)),
            },
            Order::Unsubscribe(key) => (key, bridge.unsubscribe(&key.host.0, key.pane)),
        };
        if let Err(error) = result {
            subscriptions.not_sent(order, now);
            failures.push((key.host.clone(), error.to_string()));
        }
    }
    failures
}

impl WindowShell {
    /// Move each pane's standing on what a host said about carrying it.
    pub(crate) fn note_answer(&mut self, said: &ManagerEvent, context: &mut Context<'_, Self>) {
        match said {
            ManagerEvent::Carried {
                host,
                pane,
                sequence,
            } => {
                let key = pane_key(host, *pane);
                self.subscriptions.carried(&key);
                // Carried from the byte the emulator will stand at, the host
                // sends no screen: a gap after this asks for one at once.
                if self.subscriptions.resume_from(&key) == Some(*sequence) {
                    self.hosts.bridge().forget_screen(&key);
                }
            }
            ManagerEvent::Screen { host, pane, .. } => {
                self.subscriptions.carried(&pane_key(host, *pane));
            }
            ManagerEvent::Detached { host, pane } => self.subscriptions.detached(&PaneKey {
                host: host.clone(),
                pane: *pane,
            }),
            ManagerEvent::Notify(Notification::Refused { host, code, .. }) => {
                let shown = self.shown_panes();
                let orders = self
                    .subscriptions
                    .refused(host, *code, &shown, Instant::now());
                self.send_orders(&orders, context);
            }
            _ => {}
        }
    }

    /// Hand subscription orders to the engine, reporting any it would not take.
    pub(crate) fn send_orders(&mut self, orders: &[Order], context: &mut Context<'_, Self>) {
        let failures = send(
            &mut self.subscriptions,
            self.hosts.bridge(),
            orders,
            Instant::now(),
        );
        for (host, detail) in failures {
            self.failure(&host, detail, context);
        }
    }
}
