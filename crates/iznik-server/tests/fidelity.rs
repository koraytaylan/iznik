//! The fidelity argument, in process: what a program writes is what the pane's
//! history holds, byte for byte, and what a fresh emulator reproduces on screen,
//! for every construct that breaks a naive implementation, and at flood volume.
//! The same is proven through the static musl binary on a real-host image by the
//! `fidelity-suite` scenarios; these are the fast in-process proofs.

use std::time::{Duration, Instant};

use iznik_protocol::identity::Sequence;
use iznik_server::pane::Pane;
use iznik_server::pty::spawn::{Program, SpawnOptions};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::corpus::{self, constructs};
use iznik_testkit::vt::Vt;

/// The pane's width.
const COLUMNS: u16 = 80;
/// The pane's height.
const ROWS: u16 = 24;
/// A history ring larger than the flood, so nothing ages out where a test needs it.
const HISTORY_BYTES: usize = 128 * 1024 * 1024;
/// How many times a settle poll retries, and how long between tries.
const POLL_ATTEMPTS: usize = 600;
/// How long a settle poll waits between tries.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A pane running `sh` that turns off output post-processing and input echo,
/// writes `path`, then idles alive so its screen stays queryable — its output is
/// exactly the file's bytes, with no prompt or translation of its own, and the
/// answer the mirror gives an unattended query is not echoed back into it.
fn raw_writer(path: &str) -> SpawnOptions {
    let script = format!("stty -opost -echo 2>/dev/null; cat {path}; exec sleep 3600");
    SpawnOptions {
        program: Program::Command {
            path: "sh".into(),
            arguments: vec!["-c".to_owned(), script],
        },
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
        terminfo_directory: None,
    }
}

/// Waits until the pane's newest sequence stops advancing — the child has written
/// all it will — and returns it.
async fn wait_idle(pane: &Pane) -> Sequence {
    let mut last = pane.state().newest;
    for _ in 0..POLL_ATTEMPTS {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = pane.state().newest;
        // Said something, and then said nothing more. Stillness alone is not
        // enough: on a machine with other work on it the child may not have
        // been scheduled yet, and a pane that has said nothing twice looks
        // exactly like one that has finished saying everything.
        if now == last && now > Sequence(0) {
            return now;
        }
        last = now;
    }
    last
}

/// The offset of the first differing byte, or `None` when the two are identical.
fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    let common = left.len().min(right.len());
    for index in 0..common {
        if left.get(index) != right.get(index) {
            return Some(index);
        }
    }
    if left.len() == right.len() {
        None
    } else {
        Some(common)
    }
}

/// A snapshot's screen layout — size, cursor, directory, scrollback count and the
/// text of every row — without the per-cell style attributes the formatter
/// reconstructs imperfectly for an overwritten cell (see `screen.rs`); the styles
/// a program relies on are proven by `screen_serializer_preserves_styles`.
fn layout(snapshot: &str) -> String {
    snapshot
        .lines()
        .take_while(|line| *line != "attributes")
        .filter(|line| !line.starts_with("title "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// For every corpus construct, the bytes the child wrote arrive byte-identical in
/// the pane's history, and the serialized screen reproduces the construct's screen
/// through a fresh emulator.
///
/// # Panics
///
/// When a construct is not byte-identical in history or does not reproduce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fidelity_is_byte_identical_and_reproduces_every_construct() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let path = std::env::temp_dir().join("iznik-fidelity-construct.bin");
    for construct in constructs() {
        // Graphics are images a client re-requests, not the text screen this
        // proves; the serializer skips them and so does this.
        if construct.name.contains("graphics") {
            continue;
        }
        let content: Vec<u8> = construct.chunks.concat();
        std::fs::write(&path, &content).expect("the construct is written");

        let pane = Pane::spawn(&raw_writer(&path.to_string_lossy()), HISTORY_BYTES, &thread)
            .await
            .expect("a pane spawns");
        wait_idle(&pane).await;

        let history = pane.read_history(Sequence(0)).expect("history from zero");
        assert_eq!(
            first_difference(&history, &content),
            None,
            "construct {} is not byte-identical: history {} bytes, wrote {} bytes",
            construct.name,
            history.len(),
            content.len()
        );

        let screen = pane.screen().await.expect("a screen");
        let mut reproduced = Vt::new(screen.columns, screen.rows).expect("a reproduction");
        reproduced.feed(&screen.bytes);
        let mut direct = Vt::new(COLUMNS, ROWS).expect("a direct oracle");
        direct.feed(&content);
        assert_eq!(
            layout(&reproduced.snapshot().expect("a reproduced snapshot")),
            layout(&direct.snapshot().expect("a direct snapshot")),
            "construct {} does not reproduce",
            construct.name
        );
    }
    let _removed = std::fs::remove_file(&path);
}

/// A sixty-four mebibyte flood arrives byte-identical from history where the ring
/// still holds it, `newest` is the total length, the ring holds no more than its
/// capacity, and the whole runs well within five seconds.
///
/// # Panics
///
/// When the flood is not held faithfully or takes too long.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fidelity_survives_a_flood() {
    const FLOOD_BYTES: usize = 64 * 1024 * 1024;
    const RING_BYTES: usize = 8 * 1024 * 1024;
    let started = Instant::now();

    let thread = MirrorThread::start().expect("the mirror thread starts");
    let content = corpus::generated(0xf100d, FLOOD_BYTES);
    let path = std::env::temp_dir().join("iznik-fidelity-flood.bin");
    std::fs::write(&path, &content).expect("the flood is written");

    let pane = Pane::spawn(&raw_writer(&path.to_string_lossy()), RING_BYTES, &thread)
        .await
        .expect("a pane spawns");
    let newest = wait_idle(&pane).await;
    let _removed = std::fs::remove_file(&path);

    assert_eq!(
        newest.0,
        u64::try_from(FLOOD_BYTES).unwrap_or(u64::MAX),
        "newest is the total length written"
    );

    let state = pane.state();
    let held = newest.0.saturating_sub(state.oldest.0);
    assert!(
        held <= u64::try_from(RING_BYTES).unwrap_or(u64::MAX),
        "the ring holds no more than its capacity: {held} <= {RING_BYTES}"
    );

    // The bytes the ring still holds are byte-identical to the tail of the flood.
    let tail = pane
        .read_history(state.oldest)
        .expect("history from the oldest held");
    let start = usize::try_from(state.oldest.0).unwrap_or(content.len());
    let expected = content.get(start..).unwrap_or(&[]);
    assert_eq!(
        first_difference(&tail, expected),
        None,
        "the held tail is byte-identical to the flood"
    );

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the flood is held within five seconds: {:?}",
        started.elapsed()
    );
}
