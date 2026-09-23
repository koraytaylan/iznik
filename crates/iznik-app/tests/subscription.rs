//! A pane's carrying moves on what the host answers, not on the order being
//! queued: a refusal is asked again after a growing wait, a detach is asked
//! again at once, and running out of channels frees the hidden panes' ones.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use iznik_app::subscription::{
    FIRST_RETRY, HIDDEN_KEPT, LONGEST_RETRY, Order, Standing, Subscriptions,
};
use iznik_app::vt::PaneKey;
use iznik_client::host::identity::HostId;
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::ErrorCode;

/// The host every fixture pane is on.
const HOST: &str = "far";
/// Another host, whose panes a refusal from the first must not touch.
const OTHER_HOST: &str = "near";
/// More refusals in a row than it takes the wait to reach its longest.
const MANY_REFUSALS: usize = 12;

/// A pane on a host.
fn key(host: &str, pane: u64) -> PaneKey {
    PaneKey {
        host: HostId(host.to_owned()),
        pane: PaneId(pane),
    }
}

/// The set of panes shown.
fn shown(keys: &[&PaneKey]) -> BTreeSet<PaneKey> {
    keys.iter().map(|key| (*key).clone()).collect()
}

/// A subscription refused for want of a channel is asked again once its wait
/// is over, and not before; each refusal in a row waits longer, up to a limit.
///
/// # Panics
///
/// When an order is sent early, late or twice.
#[test]
fn a_refused_subscription_is_asked_again_after_a_growing_wait() {
    let pane = key(HOST, 1);
    let visible = shown(&[&pane]);
    let subscribe = vec![Order::Subscribe(pane.clone())];
    let mut subscriptions = Subscriptions::new();
    let start = Instant::now();
    assert_eq!(
        subscriptions.plan(&visible, start),
        subscribe,
        "a shown pane is asked for"
    );
    assert!(
        subscriptions.plan(&visible, start).is_empty(),
        "queued is not carried, and nothing more is asked while the answer is out"
    );
    let host = HostId(HOST.to_owned());
    subscriptions.refused(&host, ErrorCode::ChannelsExhausted, &visible, start);
    assert!(
        matches!(subscriptions.standing(&pane), Standing::Refused { .. }),
        "the refusal is recorded"
    );
    assert!(
        subscriptions.plan(&visible, start).is_empty(),
        "not asked again before the wait"
    );
    let later = start + FIRST_RETRY;
    assert_eq!(
        subscriptions.plan(&visible, later),
        subscribe,
        "asked again once the wait is over"
    );
    subscriptions.refused(&host, ErrorCode::ChannelsExhausted, &visible, later);
    assert!(
        subscriptions.plan(&visible, later + FIRST_RETRY).is_empty(),
        "the second refusal waits twice as long as the first"
    );
    let mut now = later + FIRST_RETRY + FIRST_RETRY;
    assert_eq!(
        subscriptions.plan(&visible, now),
        subscribe,
        "asked again after the doubled wait"
    );
    for _ in 0..MANY_REFUSALS {
        subscriptions.refused(&host, ErrorCode::ChannelsExhausted, &visible, now);
        now += LONGEST_RETRY;
        assert_eq!(
            subscriptions.plan(&visible, now),
            subscribe,
            "no wait is longer than the longest"
        );
    }
    subscriptions.screen(&pane);
    assert_eq!(
        subscriptions.standing(&pane),
        Standing::Carried,
        "a screen is the answer"
    );
    assert!(
        subscriptions.plan(&visible, now + LONGEST_RETRY).is_empty(),
        "a carried pane is not asked for again"
    );
}

/// A pane the host stops carrying on its own is asked for again, and a detach
/// that ends an earlier stream does not undo a subscribe already asked for.
///
/// # Panics
///
/// When a detached pane is left blank or asked for twice.
#[test]
fn a_detached_pane_is_asked_for_again() {
    let pane = key(HOST, 1);
    let visible = shown(&[&pane]);
    let mut subscriptions = Subscriptions::new();
    let now = Instant::now();
    let _asked = subscriptions.plan(&visible, now);
    subscriptions.detached(&pane);
    assert!(
        matches!(subscriptions.standing(&pane), Standing::Asked { .. }),
        "a detach before the answer ends an older stream"
    );
    subscriptions.screen(&pane);
    subscriptions.detached(&pane);
    assert_eq!(
        subscriptions.standing(&pane),
        Standing::Idle,
        "a detach ends the carrying"
    );
    assert_eq!(
        subscriptions.plan(&visible, now),
        vec![Order::Subscribe(pane.clone())],
        "a shown pane that was detached is asked for again at once"
    );
}

/// Running out of channels lets the hidden panes on that host go, and leaves
/// the shown panes and every other host's panes as they were.
///
/// # Panics
///
/// When a shown pane or another host's pane is let go.
#[test]
fn running_out_of_channels_frees_hidden_panes_first() {
    let shown_pane = key(HOST, 1);
    let hidden_pane = key(HOST, 2);
    let waiting_pane = key(HOST, 3);
    let elsewhere = key(OTHER_HOST, 1);
    let mut subscriptions = Subscriptions::new();
    let now = Instant::now();
    let carried = shown(&[&shown_pane, &hidden_pane, &elsewhere]);
    let _asked = subscriptions.plan(&carried, now);
    for pane in [&shown_pane, &hidden_pane, &elsewhere] {
        subscriptions.screen(pane);
    }
    let _hidden_too = subscriptions.plan(
        &shown(&[&shown_pane, &hidden_pane, &elsewhere, &waiting_pane]),
        now,
    );
    // The refusal lands while the hidden pane still holds its channel.
    let host = HostId(HOST.to_owned());
    let visible = shown(&[&shown_pane, &waiting_pane]);
    let freed = subscriptions.refused(&host, ErrorCode::ChannelsExhausted, &visible, now);
    assert_eq!(
        freed,
        vec![Order::Unsubscribe(hidden_pane.clone())],
        "the hidden pane gives its channel back"
    );
    assert_eq!(
        subscriptions.standing(&hidden_pane),
        Standing::Idle,
        "and is no longer carried"
    );
    assert!(
        matches!(
            subscriptions.standing(&waiting_pane),
            Standing::Refused { .. }
        ),
        "the waiting pane is put off"
    );
    assert_eq!(
        subscriptions.standing(&shown_pane),
        Standing::Carried,
        "a shown pane keeps its channel"
    );
    assert_eq!(
        subscriptions.standing(&elsewhere),
        Standing::Carried,
        "another host's pane is untouched"
    );
}

/// A refusal that is not about subscribing moves nothing, and a refused pane
/// no longer shown stops waiting.
///
/// # Panics
///
/// When an input backlog puts a subscription off.
#[test]
fn other_refusals_leave_subscriptions_alone() {
    let pane = key(HOST, 1);
    let visible = shown(&[&pane]);
    let mut subscriptions = Subscriptions::new();
    let now = Instant::now();
    let _asked = subscriptions.plan(&visible, now);
    let host = HostId(HOST.to_owned());
    let freed = subscriptions.refused(&host, ErrorCode::InputBacklog, &visible, now);
    assert!(freed.is_empty(), "an input backlog frees nothing");
    assert!(
        matches!(
            subscriptions.standing(&pane),
            Standing::Asked { wait } if wait == FIRST_RETRY
        ),
        "an input backlog puts nothing off"
    );
    let _refused = subscriptions.refused(&host, ErrorCode::UnknownPane, &visible, now);
    assert!(
        subscriptions
            .plan(&BTreeSet::new(), now + Duration::from_secs(1))
            .is_empty(),
        "a refused pane that is hidden needs no order"
    );
    assert_eq!(
        subscriptions.standing(&pane),
        Standing::Idle,
        "and stops waiting"
    );
}

/// Going back to a tab finds its panes still carried, so no subscribe and no
/// whole screen is asked for; only panes past the most recent few hidden ones
/// are let go, the longest hidden first.
///
/// # Panics
///
/// When a recently hidden pane is subscribed again or an old one is kept.
#[test]
fn a_recently_hidden_pane_stays_carried() {
    let first = key(HOST, 0);
    let mut subscriptions = Subscriptions::new();
    let mut now = Instant::now();
    let _shown = subscriptions.plan(&shown(&[&first]), now);
    subscriptions.screen(&first);
    let mut panes = Vec::new();
    for pane in 1..=HIDDEN_KEPT {
        let next = key(HOST, u64::try_from(pane).unwrap_or(u64::MAX));
        now += FIRST_RETRY;
        let orders = subscriptions.plan(&shown(&[&next]), now);
        assert_eq!(
            orders,
            vec![Order::Subscribe(next.clone())],
            "switching tabs subscribes the new pane and keeps the old"
        );
        subscriptions.screen(&next);
        panes.push(next);
    }
    assert_eq!(
        subscriptions.standing(&first),
        Standing::Carried,
        "the first pane is still among the kept"
    );
    now += FIRST_RETRY;
    let returned = subscriptions.plan(&shown(&[&first]), now);
    assert!(
        returned.is_empty(),
        "going back to a kept pane asks for nothing: {returned:?}"
    );
    let newest = key(HOST, u64::MAX);
    now += FIRST_RETRY;
    let orders = subscriptions.plan(&shown(&[&newest]), now);
    let oldest = panes.first().cloned().unwrap_or_else(|| first.clone());
    assert_eq!(
        orders,
        vec![
            Order::Subscribe(newest.clone()),
            Order::Unsubscribe(oldest.clone())
        ],
        "one pane too many hidden lets go of the longest hidden"
    );
    assert_eq!(
        subscriptions.standing(&first),
        Standing::Carried,
        "the pane shown a moment ago is kept"
    );
}
