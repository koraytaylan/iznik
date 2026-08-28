//! The pure tables the pump is built on: channels handed out lowest first and
//! held back until the client says the wire is clear, credit that cannot be
//! overdrawn or overflowed, and constants that keep their relation to each
//! other so a later edit cannot make a stale mark unreachable.

use iznik_protocol::frame::{MAXIMUM_PAYLOAD_LENGTH, encode};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_server::multiplexer::channel::{ChannelTable, Cursor, MultiplexerError, SinkError};
use iznik_server::multiplexer::credit::{
    CreditWindow, FOCUSED_CREDIT_BYTES, FRAME_PAYLOAD_LENGTH, INITIAL_CREDIT_BYTES,
    STALE_THRESHOLD_BYTES,
};

/// How many channels pane output may flow on: every byte but the control
/// channel's zero.
const PANE_CHANNELS: u64 = 255;

/// Subscriptions receive distinct channels from one upward, the table agrees
/// with itself in both directions, and the two hundred and fifty-sixth is
/// refused.
///
/// # Panics
///
/// When a channel repeats, the table disagrees with itself, or the refusal
/// comes at the wrong time.
#[test]
fn channel_multiplexer_hands_out_every_channel_once_and_then_refuses() {
    let mut table = ChannelTable::new();
    let mut handed = Vec::new();
    for index in 1..=PANE_CHANNELS {
        let pane = PaneId(index);
        let channel = table
            .assign(pane)
            .unwrap_or_else(|error| panic!("pane {index}: {error}"));
        assert_eq!(
            u64::from(channel),
            index,
            "channels are handed out lowest first"
        );
        assert_eq!(table.channel_of(pane), Some(channel), "pane {index}");
        assert_eq!(table.pane_of(channel), Some(pane), "channel {channel}");
        handed.push(channel);
    }
    handed.sort_unstable();
    handed.dedup();
    assert_eq!(
        u64::try_from(handed.len()).unwrap_or(0),
        PANE_CHANNELS,
        "every channel was handed out once"
    );
    assert_eq!(
        table.assign(PaneId(0)),
        Err(MultiplexerError::ChannelsExhausted),
        "the two hundred and fifty-sixth"
    );
    assert_eq!(
        table.channel_of(PaneId(0)),
        None,
        "a refusal assigns nothing"
    );
}

/// A released channel is not handed out again until the client acknowledges
/// it, so a frame still on the wire cannot be painted into a new pane.
///
/// # Panics
///
/// When a released channel is reused early, or an acknowledgement of a channel
/// that was not released is accepted.
#[test]
fn channel_multiplexer_holds_a_released_channel_until_it_is_acknowledged() {
    let mut table = ChannelTable::new();
    let first = table.assign(PaneId(1)).expect("a channel");
    let second = table.assign(PaneId(2)).expect("a channel");
    assert_eq!((first, second), (1, 2), "lowest first");

    table.release(first);
    assert_eq!(table.channel_of(PaneId(1)), None, "the pane is detached");
    assert_eq!(table.pane_of(first), None, "the channel carries nothing");
    assert_eq!(table.awaiting_acknowledgement(), 1, "one is held back");

    let third = table.assign(PaneId(3)).expect("a channel");
    assert_ne!(third, first, "a released channel was handed out early");
    assert_eq!(third, 3, "the next free channel, not the released one");

    table.acknowledge(first).expect("the client acknowledges");
    assert_eq!(table.awaiting_acknowledgement(), 0, "nothing is held back");
    assert_eq!(
        table.assign(PaneId(4)).expect("a channel"),
        first,
        "an acknowledged channel is free again"
    );

    assert_eq!(
        table.acknowledge(first),
        Err(MultiplexerError::NotReleased { channel: first }),
        "acknowledging a channel that was not released names it"
    );
    assert_eq!(
        table.acknowledge(200),
        Err(MultiplexerError::NotReleased { channel: 200 }),
        "and so does acknowledging one nothing ever had"
    );
    table.release(199);
    assert_eq!(
        table.awaiting_acknowledgement(),
        0,
        "releasing a channel nothing is assigned to holds nothing back"
    );
}

/// Credit cannot be overdrawn, overflowed, or inflated past what the client
/// said it can hold — and moving focus keeps the bytes already in flight
/// counting against that.
///
/// # Panics
///
/// When a window gives out more than it has, wraps, grows past its ceiling, or
/// lets focus put more in flight than the client can hold.
#[test]
fn channel_multiplexer_credit_cannot_be_overdrawn_or_overflowed() {
    assert_eq!(
        CreditWindow::background().available(),
        INITIAL_CREDIT_BYTES,
        "a background window"
    );
    assert_eq!(
        CreditWindow::focused().available(),
        FOCUSED_CREDIT_BYTES,
        "a focused window"
    );

    let mut window = CreditWindow::background();
    assert_eq!(window.consume(1024), 1024, "it gives what it has");
    assert_eq!(
        window.available(),
        INITIAL_CREDIT_BYTES.saturating_sub(1024),
        "and keeps the rest"
    );
    assert_eq!(
        window.consume(u32::MAX),
        INITIAL_CREDIT_BYTES.saturating_sub(1024),
        "it never gives more than it has"
    );
    assert_eq!(window.available(), 0, "and is then empty");
    assert_eq!(window.consume(1), 0, "an empty window gives nothing");

    window.refill(u32::MAX);
    window.refill(u32::MAX);
    assert_eq!(
        window.available(),
        INITIAL_CREDIT_BYTES,
        "a client that returns more credit than it was sent does not widen the window"
    );
}

/// Moving focus moves the larger window without letting the client hold more
/// than it said it can, counting what is already on its way to it.
///
/// # Panics
///
/// When focus puts more in flight than the ceiling it moves to.
#[test]
fn channel_multiplexer_focus_moves_the_window_without_outrunning_the_client() {
    // A drawn-down background window has a whole background window
    // outstanding at the client.
    let mut drawn = CreditWindow::background();
    let outstanding = drawn.consume(INITIAL_CREDIT_BYTES);
    assert_eq!(
        outstanding, INITIAL_CREDIT_BYTES,
        "the whole window went out"
    );
    drawn.widen();
    assert_eq!(drawn.ceiling(), FOCUSED_CREDIT_BYTES, "the focused ceiling");
    assert_eq!(
        outstanding.saturating_add(drawn.available()),
        FOCUSED_CREDIT_BYTES,
        "focus put more in flight than a focused client can hold"
    );
    drawn.narrow();
    assert_eq!(
        drawn.ceiling(),
        INITIAL_CREDIT_BYTES,
        "the background ceiling"
    );
    assert_eq!(
        outstanding.saturating_add(drawn.available()),
        INITIAL_CREDIT_BYTES,
        "focus leaving left more in flight than a background client can hold"
    );

    let mut fresh = CreditWindow::background();
    fresh.widen();
    assert_eq!(
        fresh.available(),
        FOCUSED_CREDIT_BYTES,
        "focus moves the larger window here"
    );
    fresh.narrow();
    assert_eq!(
        fresh.available(),
        INITIAL_CREDIT_BYTES,
        "and takes it away again"
    );
}

/// A pane asked for twice keeps the channel it has: a repaint and a resume are
/// both subscriptions, and a second channel would leave the table disagreeing
/// with itself and the first channel unreachable.
///
/// # Panics
///
/// When a second request hands out a second channel.
#[test]
fn channel_multiplexer_a_pane_asked_for_twice_keeps_its_channel() {
    let mut table = ChannelTable::new();
    let first = table.assign(PaneId(1)).expect("a channel");
    let again = table.assign(PaneId(1)).expect("the same channel");
    assert_eq!(again, first, "a pane was given a second channel");
    assert_eq!(table.assigned(), 1, "and the table counted it twice");
    assert_eq!(table.channel_of(PaneId(1)), Some(first), "one way");
    assert_eq!(table.pane_of(first), Some(PaneId(1)), "and the other");
    table.release(first);
    table.acknowledge(first).expect("the client acknowledges");
    assert_eq!(
        table.assign(PaneId(2)).expect("a channel"),
        first,
        "the channel came back, so nothing leaked"
    );
}

/// A cursor knows how far behind the pane it is, and nothing before the pane's
/// newest byte reads as being ahead of it.
///
/// # Panics
///
/// When the lag is wrong.
#[test]
fn channel_multiplexer_a_cursor_knows_how_far_behind_it_is() {
    let cursor = Cursor::new(PaneId(1), 1, Sequence(1_000));
    assert_eq!(
        cursor.credit.available(),
        INITIAL_CREDIT_BYTES,
        "background"
    );
    assert!(!cursor.stale, "a new cursor is not stale");
    assert_eq!(cursor.lag(Sequence(1_000)), 0, "level with the pane");
    assert_eq!(cursor.lag(Sequence(1_500)), 500, "five hundred behind");
    assert_eq!(cursor.lag(Sequence(0)), 0, "never behind by less than none");
}

/// The constants keep their relation to each other, and the test says so by
/// exercising them rather than by comparing two numbers the compiler has
/// already folded: a full pane frame really does fit a protocol frame, a
/// background window really does carry one, the focused window really is the
/// larger, and a cursor cannot be marked stale while its own window would
/// still have carried it.
///
/// # Panics
///
/// When a relation stops holding.
#[test]
fn channel_multiplexer_the_constants_keep_their_relation() {
    let payload = vec![0; usize::try_from(FRAME_PAYLOAD_LENGTH).unwrap_or(0)];
    let mut framed = Vec::new();
    encode(1, &payload, &mut framed).expect("a full pane frame fits a protocol frame");
    let over = vec![0; usize::try_from(MAXIMUM_PAYLOAD_LENGTH).unwrap_or(0) + 1];
    let mut refused = Vec::new();
    assert!(
        encode(1, &over, &mut refused).is_err(),
        "the protocol frame has no ceiling to be under"
    );

    let mut background = CreditWindow::background();
    assert_eq!(
        background.consume(FRAME_PAYLOAD_LENGTH),
        FRAME_PAYLOAD_LENGTH,
        "a background window cannot carry one whole frame"
    );
    assert!(
        CreditWindow::focused().available() > CreditWindow::background().available(),
        "the focused window is not the larger one"
    );
    let focused = CreditWindow::focused().available();
    assert!(
        u64::from(focused) <= STALE_THRESHOLD_BYTES,
        "a cursor would be marked stale while its own window of {focused} would still carry it"
    );
}

/// A link failure is a multiplexer refusal, and each says what it was.
///
/// # Panics
///
/// When a refusal does not carry what it was given.
#[test]
fn channel_multiplexer_a_link_failure_is_a_refusal() {
    assert_eq!(
        MultiplexerError::from(SinkError::Closed),
        MultiplexerError::Sink(SinkError::Closed),
        "a closed link"
    );
    let refused = SinkError::Refused {
        detail: "the frame is too large".to_owned(),
    };
    assert_eq!(
        MultiplexerError::from(refused.clone()),
        MultiplexerError::Sink(refused),
        "a refused frame"
    );
    assert!(
        MultiplexerError::UnknownPane { pane: PaneId(9) }
            .to_string()
            .contains('9'),
        "a refusal names the pane"
    );
    assert!(
        MultiplexerError::NotReleased { channel: 7 }
            .to_string()
            .contains('7'),
        "a refusal names the channel"
    );
}
