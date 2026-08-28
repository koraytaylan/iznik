//! What the client knows, held to the things that make a resume possible.
//!
//! Every case here is a value. The model does no I/O, so hosts being
//! independent, a cursor moving only forwards and a pending command being
//! found by its number are all things a test can assert exactly — which is
//! what makes the reducer and the manager above it worth trusting.

use core::time::Duration;
use std::time::Instant;

use iznik_client::host::identity::HostId;
use iznik_client::model::{ClientModel, HostView, PendingCommand, Subscription};
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab};

/// The width a pane is made at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// The channel a pane's bytes arrive on.
const CHANNEL: u8 = 3;

/// How far into a stream a subscription starts.
const FROM: Sequence = Sequence(1_000);

/// How many bytes arrive at once.
const ARRIVED: u64 = 512;

/// How much credit is given at once.
const CREDIT: u64 = 64 * 1024;

/// The pane these cases subscribe to.
const PANE: PaneId = PaneId(1);

/// Another, for the cases about two of them.
const OTHER: PaneId = PaneId(2);

/// A host model with one session, one tab and one pane in it.
fn one_pane(generation: u64) -> HostModel {
    HostModel {
        generation: Generation(generation),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                panes: vec![Pane {
                    id: PANE,
                    title: String::new(),
                    working_directory: None,
                    columns: COLUMNS,
                    rows: ROWS,
                }],
                layout: LayoutNode::Leaf(PANE),
            }],
        }],
    }
}

/// A command sent at `moment` and not yet answered.
fn sent(id: u64, name: &str, moment: Instant) -> PendingCommand {
    PendingCommand {
        id: CommandId(id),
        command: SessionCommand::RenameSession {
            session: SessionId(1),
            name: name.to_owned(),
        },
        answered: None,
        submitted_at: moment,
    }
}

/// # Panics
///
/// When a model that knows nothing is not empty.
#[test]
fn client_model_starts_knowing_nothing() {
    let model = ClientModel::default();
    assert!(model.is_empty(), "it knows no hosts");
    assert_eq!(model.aliases(), Vec::<&HostId>::new(), "and lists none");
    assert_eq!(model.subscribed(), 0, "and subscribes to nothing");
    assert_eq!(model.validate(), Ok(()), "and holds nothing that is wrong");
}

/// # Panics
///
/// When touching one host's view changes another's.
#[test]
fn client_model_keeps_its_hosts_independent() {
    let mut model = ClientModel::default();
    let work = HostId("work".to_owned());
    let build = HostId("build".to_owned());
    let spare = HostId("spare".to_owned());
    for (alias, generation) in [(&work, 1), (&build, 2), (&spare, 3)] {
        let _first = model.insert(alias.clone(), HostView::of(one_pane(generation)));
    }
    // What the others look like before anything happens to `work`.
    let untouched: Vec<HostView> = [&build, &spare]
        .into_iter()
        .filter_map(|alias| model.host(alias).cloned())
        .collect();
    // Adding to one, replacing one, and taking one away.
    if let Some(view) = model.host_mut(&work) {
        let _opened = view.subscribe(PANE, CHANNEL, FROM);
        view.focus = Some(PANE);
    }
    let _replaced = model.insert(work.clone(), HostView::of(one_pane(9)));
    let _removed = model.remove(&work);
    let after: Vec<HostView> = [&build, &spare]
        .into_iter()
        .filter_map(|alias| model.host(alias).cloned())
        .collect();
    assert_eq!(
        after, untouched,
        "adding to, replacing and removing one host leaves the others exactly as they were"
    );
    assert_eq!(model.host(&work), None, "and the one removed is gone");
    assert_eq!(
        model.aliases(),
        vec![&build, &spare],
        "listed by the alias their user gave them"
    );
}

/// # Panics
///
/// When a subscription does not record what the host announced, or its cursor
/// moves backwards.
#[test]
fn client_model_advances_a_cursor_exactly_and_forwards() {
    let mut view = HostView::of(one_pane(1));
    let opened = view.subscribe(PANE, CHANNEL, FROM);
    assert_eq!(
        opened,
        Subscription {
            channel: CHANNEL,
            cursor: FROM,
            credit_outstanding: 0,
        },
        "it records the channel the host announced and the byte it starts at"
    );
    let Some(held) = view.subscription_mut(PANE) else {
        panic!("the subscription just opened is there");
    };
    assert_eq!(
        held.advance(ARRIVED),
        Sequence(FROM.0.saturating_add(ARRIVED)),
        "and moves on by exactly what arrived"
    );
    assert_eq!(
        held.advance(0),
        Sequence(FROM.0.saturating_add(ARRIVED)),
        "nothing arriving moves it nowhere"
    );
    // The cursor is what a resume asks from: wrapping it would ask a host for
    // the beginning of a stream it has long since dropped.
    held.cursor = Sequence(u64::MAX);
    assert_eq!(
        held.advance(ARRIVED),
        Sequence(u64::MAX),
        "and it never wraps past the end"
    );
}

/// # Panics
///
/// When a screen the server sent does not put the cursor where it says.
#[test]
fn client_model_puts_the_cursor_where_a_screen_says() {
    let mut view = HostView::of(one_pane(1));
    let _opened = view.subscribe(PANE, CHANNEL, FROM);
    let Some(held) = view.subscription_mut(PANE) else {
        panic!("the subscription just opened is there");
    };
    let _moved = held.advance(ARRIVED);
    // A client too far behind to be caught up byte by byte is given a screen
    // and the sequence it stands at, and that is the one thing that may move
    // the cursor backwards — because the server said so.
    held.resume_at(FROM);
    assert_eq!(held.cursor, FROM, "the screen decides where it stands");
}

/// # Panics
///
/// When credit given and credit spent do not add up.
#[test]
fn client_model_counts_the_credit_it_has_given() {
    let mut held = Subscription::opened(CHANNEL, FROM);
    held.grant(CREDIT);
    assert_eq!(held.credit_outstanding, CREDIT, "what was given");
    held.spend(ARRIVED);
    assert_eq!(
        held.credit_outstanding,
        CREDIT.saturating_sub(ARRIVED),
        "less what was spent"
    );
    held.spend(CREDIT);
    assert_eq!(
        held.credit_outstanding, 0,
        "and a server that spent more than it was given leaves none, not a debt \
         that would read as a grant of sixteen exabytes"
    );
}

/// # Panics
///
/// When unsubscribing leaves the pane focused.
#[test]
fn client_model_drops_the_focus_with_the_subscription() {
    let mut view = HostView::of(one_pane(1));
    let _opened = view.subscribe(PANE, CHANNEL, FROM);
    view.focus = Some(PANE);
    let dropped = view.unsubscribe(PANE);
    assert_eq!(
        dropped,
        Some(Subscription::opened(CHANNEL, FROM)),
        "it says what the subscription was"
    );
    assert_eq!(
        view.focus, None,
        "and nothing is focused on a pane there is no subscription to"
    );
    assert_eq!(view.unsubscribe(PANE), None, "and a second time says so");
}

/// # Panics
///
/// When a second announcement for one pane does not replace the first.
#[test]
fn client_model_takes_the_hosts_latest_announcement() {
    let mut view = HostView::of(one_pane(1));
    let _first = view.subscribe(PANE, CHANNEL, FROM);
    // A reconnection announces the pane again, on whatever channel is free
    // and from wherever the resume began.
    let second = view.subscribe(
        PANE,
        CHANNEL.saturating_add(1),
        Sequence(FROM.0.saturating_add(ARRIVED)),
    );
    assert_eq!(
        view.subscription(PANE),
        Some(&second),
        "the later one stands"
    );
    assert_eq!(view.subscriptions.len(), 1, "and there is one of it");
}

/// # Panics
///
/// When pending commands are not in the order they were sent, or cannot be
/// found by their number.
#[test]
fn client_model_orders_pending_commands_by_submission() {
    let start = Instant::now();
    let mut view = HostView::of(one_pane(1));
    let moments: Vec<Instant> = (0..3)
        .map(|step| {
            start
                .checked_add(Duration::from_millis(step))
                .unwrap_or(start)
        })
        .collect();
    for (index, moment) in moments.iter().enumerate() {
        let number = u64::try_from(index).unwrap_or(0) + 1;
        view.record(sent(number, "renamed", *moment));
    }
    assert_eq!(
        view.pending
            .iter()
            .map(|held| held.id.0)
            .collect::<Vec<u64>>(),
        vec![1, 2, 3],
        "in the order they were sent, which is the order they roll back in"
    );
    assert_eq!(
        view.awaiting(CommandId(2)).map(|held| held.submitted_at),
        moments.get(1).copied(),
        "and each is found by its number"
    );
    assert_eq!(
        view.awaiting(CommandId(9)),
        None,
        "and one that was never sent is not"
    );
    let retired = view.retire(CommandId(2));
    assert_eq!(
        retired.map(|held| held.id),
        Some(CommandId(2)),
        "retiring gives it back"
    );
    assert_eq!(
        view.pending
            .iter()
            .map(|held| held.id.0)
            .collect::<Vec<u64>>(),
        vec![1, 3],
        "and leaves the rest in order"
    );
    assert_eq!(view.retire(CommandId(2)), None, "and a second time says so");
}

/// # Panics
///
/// When the commands old enough to have been given up on are not the ones
/// named.
#[test]
fn client_model_names_the_commands_that_have_waited() {
    let start = Instant::now();
    let later = start.checked_add(Duration::from_secs(1)).unwrap_or(start);
    let mut view = HostView::of(one_pane(1));
    view.record(sent(1, "early", start));
    view.record(sent(2, "late", later));
    assert_eq!(
        view.sent_before(later),
        vec![CommandId(1)],
        "only the one sent before the moment asked about"
    );
}

/// # Panics
///
/// When a host model that could not have come from a server is held without
/// complaint, or the complaint does not say which host.
#[test]
fn client_model_validates_every_host_it_holds() {
    let mut model = ClientModel::default();
    let good = HostId("work".to_owned());
    let bad = HostId("build".to_owned());
    let _first = model.insert(good.clone(), HostView::of(one_pane(1)));
    assert_eq!(model.validate(), Ok(()), "a model a server could have sent");
    // A session with no tabs is one no server produces, and a client that
    // held one would render something the host does not have.
    let mut broken = one_pane(1);
    if let Some(session) = broken.sessions.first_mut() {
        session.tabs.clear();
    }
    let _second = model.insert(bad.clone(), HostView::of(broken));
    let Err(complaint) = model.validate() else {
        panic!("a session with no tabs was held without complaint");
    };
    assert_eq!(complaint.host, bad, "and the complaint says which host");
    assert!(
        complaint.to_string().contains("build"),
        "and reads as one: {complaint}"
    );
}

/// # Panics
///
/// When a command a replaced daemon answered goes on being shown, or one it
/// never answered is thrown away with it.
#[test]
fn client_model_lets_go_of_what_a_replaced_daemon_answered() {
    let now = Instant::now();
    let mut view = HostView::of(one_pane(5));
    // One answered at the generation the host said it reached, and one still
    // waiting. The first is what the host is about to stop being able to
    // announce.
    let mut answered = sent(1, "answered", now);
    answered.answered = Some(Generation(5));
    view.record(answered);
    view.record(sent(2, "waiting", now));
    // A daemon that was upgraded, or restarted, begins again at nothing: a
    // generation below the one settled is another host's first word, not this
    // one's next.
    let abandoned = view.settle(one_pane(0));
    assert_eq!(
        abandoned,
        vec![CommandId(1)],
        "the answered one is given up on, and named"
    );
    assert_eq!(
        view.pending.iter().map(|held| held.id).collect::<Vec<_>>(),
        vec![CommandId(2)],
        "and the one nobody answered stays, to time out in its own time"
    );
}

/// # Panics
///
/// When a host that carried on from where it was loses a command that is
/// still in flight.
#[test]
fn client_model_keeps_what_is_in_flight_across_a_snapshot() {
    let now = Instant::now();
    let mut view = HostView::of(one_pane(5));
    let mut answered = sent(1, "answered", now);
    answered.answered = Some(Generation(7));
    view.record(answered);
    // The same host, further on: everything in flight is still in flight.
    let abandoned = view.settle(one_pane(6));
    assert!(
        abandoned.is_empty(),
        "nothing is given up on when a host carries on"
    );
    assert_eq!(view.pending.len(), 1, "and what was in flight still is");
}

/// # Panics
///
/// When a pane whose channel another pane took is still said to have one.
#[test]
fn client_model_says_a_pane_has_no_channel_once_another_takes_it() {
    let mut view = HostView::of(one_pane(1));
    let _first = view.subscribe(PANE, CHANNEL, FROM);
    assert_eq!(
        view.carried(PANE),
        Some(CHANNEL),
        "a pane that was given a channel has it"
    );
    // The host gives the same number to another pane, which is what a
    // reconnection re-announcing panes does.
    let _second = view.subscribe(OTHER, CHANNEL, FROM);
    assert_eq!(
        view.carried(PANE),
        None,
        "and the pane that lost it has none, rather than the control channel"
    );
    assert_eq!(
        view.carried(OTHER),
        Some(CHANNEL),
        "while the pane that took it does"
    );
}
