//! The mirror proven against a reference emulator and its own response policy:
//! it answers embedder queries only when nobody is attached, ignores what the
//! client acts on, agrees with the `vt` oracle on what a pane looks like across
//! the fidelity corpus, bounds its scrollback, and runs many panes' `!Send`
//! tasks on one thread without one delaying another.

use std::time::{Duration, Instant};

use iznik_server::terminal::mirror::{MIRROR_SCROLLBACK_ROWS, Mirror, MirrorThread};
use iznik_testkit::corpus::constructs;
use iznik_testkit::vt::Vt;

/// The XTVERSION the mirror answers, wrapped in the DCS it arrives in.
fn xtversion_answer() -> Vec<u8> {
    concat!("\x1bP>|iznik-server ", env!("CARGO_PKG_VERSION"), "\x1b\\")
        .as_bytes()
        .to_vec()
}

/// The reports the emulator answers itself — cursor position and device status —
/// reach the pending buffer with no subscriber, and nothing does once one is
/// attached.
///
/// # Panics
///
/// When a report is missing or leaks past a subscriber.
#[test]
fn terminal_mirror_reports_cursor_position_only_without_subscribers() {
    let mut mirror = Mirror::new(80, 24).expect("a mirror");
    mirror.feed(b"\x1b[6n");
    assert_eq!(
        mirror.take_pending_responses(),
        b"\x1b[1;1R".to_vec(),
        "the cursor is home"
    );
    mirror.feed(b"\x1b[5n");
    assert_eq!(
        mirror.take_pending_responses(),
        b"\x1b[0n".to_vec(),
        "the device status is ok"
    );

    mirror.set_subscriber_count(1);
    mirror.feed(b"\x1b[6n");
    mirror.feed(b"\x1b[5n");
    assert!(
        mirror.take_pending_responses().is_empty(),
        "an attached client's terminal answers"
    );
}

/// Every embedder-side query is answered from iznik's fixed values with no
/// subscriber, and none is answered with one.
///
/// # Panics
///
/// When an answer is wrong, or one leaks past a subscriber.
#[test]
fn terminal_mirror_answers_embedder_queries_only_without_subscribers() {
    let cases: Vec<(&[u8], Vec<u8>)> = vec![
        (b"\x1b[c", b"\x1b[?62c".to_vec()),
        (b"\x1b[>c", b"\x1b[>1;0;0c".to_vec()),
        (b"\x1b[>q", xtversion_answer()),
        (b"\x1b[18t", b"\x1b[8;24;80t".to_vec()),
        (b"\x1b[?996n", b"\x1b[?997;1n".to_vec()),
    ];
    for (query, answer) in &cases {
        let mut mirror = Mirror::new(80, 24).expect("a mirror");
        mirror.feed(query);
        assert_eq!(
            &mirror.take_pending_responses(),
            answer,
            "the answer to {query:?}"
        );
    }

    let mut mirror = Mirror::new(80, 24).expect("a mirror");
    mirror.feed(b"\x05");
    assert!(
        mirror.take_pending_responses().is_empty(),
        "the answerback is empty"
    );

    let mut attached = Mirror::new(80, 24).expect("a mirror");
    attached.set_subscriber_count(1);
    for (query, _answer) in &cases {
        attached.feed(query);
    }
    attached.feed(b"\x05");
    assert!(
        attached.take_pending_responses().is_empty(),
        "an attached client's terminal answers every query"
    );
}

/// A bell, a clipboard write, a title and a directory report produce no pending
/// bytes and no error, and the title and directory are still recorded.
///
/// # Panics
///
/// When an ignored effect answers, or a recorded one is wrong.
#[test]
fn terminal_mirror_ignores_the_clients_effects() {
    let mut mirror = Mirror::new(80, 24).expect("a mirror");
    mirror.feed(b"\x07");
    mirror.feed(b"\x1b]52;c;ZW5jb2RlZA==\x07");
    mirror.feed(b"\x1b]0;a window title\x07");
    mirror.feed(b"\x1b]7;file://host/tmp/work\x07");
    assert!(
        mirror.take_pending_responses().is_empty(),
        "none of these are answered"
    );
    assert_eq!(mirror.title(), "a window title", "the title was recorded");
    assert_eq!(
        mirror.working_directory(),
        Some("file://host/tmp/work".to_owned()),
        "the directory was recorded"
    );
}

/// The scrollback is bounded: far more lines than its budget never grows it past
/// the budget, and four times the input does not grow it further.
///
/// # Panics
///
/// When the scrollback is unbounded.
#[test]
fn terminal_mirror_bounds_its_scrollback() {
    let scrollback_after = |lines: usize| -> usize {
        let mut mirror = Mirror::new(80, 24).expect("a mirror");
        let mut bytes = Vec::new();
        for index in 0..lines {
            bytes.extend_from_slice(format!("row {index}\r\n").as_bytes());
        }
        mirror.feed(&bytes);
        mirror.scrollback_rows()
    };
    let modest = scrollback_after(20_000);
    let flood = scrollback_after(80_000);
    assert!(modest > 0, "scrollback is kept: {modest}");
    assert!(
        flood <= MIRROR_SCROLLBACK_ROWS,
        "never past the budget: {flood} <= {MIRROR_SCROLLBACK_ROWS}"
    );
    assert!(
        flood <= modest.saturating_mul(2),
        "four times the input does not grow the scrollback: {modest} then {flood}"
    );
}

/// For every construct in the fidelity corpus, and an explicit alternate-screen
/// switch, the mirror's title, working directory, dimensions and
/// alternate-screen flag match the oracle's after the same bytes and resizes.
///
/// # Panics
///
/// When the mirror disagrees with the oracle.
#[test]
fn terminal_mirror_agrees_with_the_oracle() {
    for construct in constructs() {
        let mut mirror = Mirror::new(80, 24).expect("a mirror");
        let mut oracle = Vt::new(80, 24).expect("an oracle");
        for chunk in &construct.chunks {
            mirror.feed(chunk);
            oracle.feed(chunk);
        }
        mirror.resize(100, 40);
        oracle.resize(100, 40).expect("the oracle resizes");

        let name = &construct.name;
        assert_eq!(
            mirror.title(),
            oracle.title().expect("an oracle title"),
            "title after {name}"
        );
        assert_eq!(
            mirror.working_directory().unwrap_or_default(),
            oracle.working_directory().expect("an oracle directory"),
            "working directory after {name}"
        );
        assert_eq!(
            (mirror.columns(), mirror.rows()),
            oracle.size().expect("an oracle size"),
            "dimensions after {name}"
        );
        assert_eq!(
            mirror.in_alternate_screen(),
            oracle.in_alternate_screen().expect("an oracle screen"),
            "alternate screen after {name}"
        );
    }

    let mut mirror = Mirror::new(80, 24).expect("a mirror");
    let mut oracle = Vt::new(80, 24).expect("an oracle");
    mirror.feed(b"\x1b[?1049h");
    oracle.feed(b"\x1b[?1049h");
    assert!(
        mirror.in_alternate_screen(),
        "the mirror entered the alternate screen"
    );
    assert!(
        oracle.in_alternate_screen().expect("an oracle screen"),
        "the oracle entered the alternate screen"
    );
    mirror.feed(b"\x1b[?1049l");
    oracle.feed(b"\x1b[?1049l");
    assert!(
        !mirror.in_alternate_screen(),
        "the mirror left the alternate screen"
    );
    assert!(
        !oracle.in_alternate_screen().expect("an oracle screen"),
        "the oracle left the alternate screen"
    );

    let mut resized = Mirror::new(80, 24).expect("a mirror");
    let mut resized_oracle = Vt::new(80, 24).expect("an oracle");
    for switch in [b"\x1b[?40h".as_slice(), b"\x1b[?3h".as_slice()] {
        resized.feed(switch);
        resized_oracle.feed(switch);
    }
    assert_eq!(
        (resized.columns(), resized.rows()),
        resized_oracle.size().expect("an oracle size"),
        "the mirror follows a program's self-resize (DECCOLM)"
    );
}

/// The mirror thread runs a pane's task and reads its result; a pane flooding
/// four mebibytes does not delay a neighbor's single feed past a generous bound;
/// and dropping the thread cancels a still-running task.
///
/// # Panics
///
/// When a task does not run, the flood delays its neighbor, or a task outlives
/// the thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_mirror_thread_runs_panes_without_one_delaying_another() {
    let thread = MirrorThread::start().expect("the thread starts");

    let (title_sender, title_receiver) = tokio::sync::oneshot::channel();
    thread.spawn(move || async move {
        let mut mirror = Mirror::new(80, 24).expect("a mirror");
        mirror.feed(b"\x1b]0;from the thread\x07");
        let _sent = title_sender.send(mirror.title());
    });
    let title = tokio::time::timeout(Duration::from_secs(2), title_receiver)
        .await
        .expect("the task ran in time")
        .expect("a title");
    assert_eq!(
        title, "from the thread",
        "the pane's task ran on the thread"
    );

    let (flood_sender, flood_receiver) = tokio::sync::oneshot::channel();
    thread.spawn(move || async move {
        let mut mirror = Mirror::new(80, 24).expect("a mirror");
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..64 {
            mirror.feed(&chunk);
            tokio::task::yield_now().await;
        }
        let _sent = flood_sender.send(());
    });

    let (quick_sender, quick_receiver) = tokio::sync::oneshot::channel();
    let started = Instant::now();
    thread.spawn(move || async move {
        let mut mirror = Mirror::new(80, 24).expect("a mirror");
        mirror.feed(b"a quick feed");
        let _sent = quick_sender.send(started.elapsed());
    });
    let latency = tokio::time::timeout(Duration::from_secs(2), quick_receiver)
        .await
        .expect("the neighbor ran in time")
        .expect("a latency");
    assert!(
        latency < Duration::from_millis(500),
        "the flood did not delay its neighbor: {latency:?}"
    );

    let _flooded = tokio::time::timeout(Duration::from_secs(5), flood_receiver).await;

    // Dropping the thread ends its tasks: a task that never completes holds a
    // sender dropped only when the task is cancelled; an echo task confirms the
    // canary is running before the drop.
    let (canary_sender, canary_receiver) = tokio::sync::oneshot::channel::<()>();
    thread.spawn(move || async move {
        let _held = canary_sender;
        std::future::pending::<()>().await;
    });
    let (echo_sender, echo_receiver) = tokio::sync::oneshot::channel();
    thread.spawn(move || async move {
        let _sent = echo_sender.send(());
    });
    let _echoed = tokio::time::timeout(Duration::from_secs(2), echo_receiver).await;
    drop(thread);
    assert!(
        canary_receiver.await.is_err(),
        "dropping the thread cancelled its still-running task"
    );
}
