//! The pane proven end to end: a real `bash` with the shell-integration asset
//! behind a `Pane`, driven by input and watched through the screen, history,
//! marks, state and exit — the four things a pane is, held together by one VT
//! task, doing what the architecture's anatomy of a pane says they do.

use std::path::Path;
use std::time::Duration;

use iznik_protocol::identity::Sequence;
use iznik_protocol::message::MarkKind;
use iznik_server::pane::Pane;
use iznik_server::pty::spawn::{Program, SpawnOptions};
use iznik_server::terminal::marks::MarkEvent;
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::corpus;
use iznik_testkit::vt::Vt;
use tokio::sync::broadcast;

/// The pane's initial width.
const COLUMNS: u16 = 80;
/// The pane's initial height.
const ROWS: u16 = 24;
/// A history ring larger than any test's output, so nothing ages out.
const HISTORY_BYTES: usize = 16 * 1024 * 1024;
/// Long enough that a real shell always finishes in time, short enough that a
/// hung test fails rather than hangs.
const DEADLINE: Duration = Duration::from_secs(20);

/// `bash` with the shell-integration asset, interactive, at `columns` by `rows`.
fn shell_options(columns: u16, rows: u16) -> SpawnOptions {
    let asset = format!(
        "{}/../iznik-testkit/assets/shell-integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    SpawnOptions {
        program: Program::Command {
            path: "bash".into(),
            arguments: vec!["--rcfile".to_owned(), asset, "-i".to_owned()],
        },
        columns,
        rows,
        working_directory: None,
        terminfo_directory: None,
    }
}

/// Whether `needle` appears in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || (needle.len() <= haystack.len()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle))
}

/// The next mark whose kind matches, within [`DEADLINE`], or `None` if the
/// deadline passes or the stream closes first.
async fn wait_kind(
    marks: &mut broadcast::Receiver<MarkEvent>,
    predicate: impl Fn(&MarkKind) -> bool,
) -> Option<MarkEvent> {
    let search = async {
        loop {
            match marks.recv().await {
                Ok(event) if predicate(&event.kind) => return Some(event),
                Ok(_other) => {}
                Err(broadcast::error::RecvError::Lagged(_count)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(DEADLINE, search).await.ok().flatten()
}

/// Whether a mark ends a command.
fn is_finished(kind: &MarkKind) -> bool {
    matches!(kind, MarkKind::CommandFinished { .. })
}

/// Whether a mark starts a prompt.
fn is_prompt(kind: &MarkKind) -> bool {
    matches!(kind, MarkKind::PromptStart)
}

/// How many times a state poll retries, and how long between tries — together a
/// little longer than [`DEADLINE`], so a real change is always seen.
const POLL_ATTEMPTS: usize = 500;
/// How long a state poll waits between tries.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Waits until the pane's newest sequence stops advancing — the shell is idle —
/// and returns it, so a read of history and of state see one consistent moment.
async fn wait_idle(pane: &Pane) -> Sequence {
    let mut last = pane.state().newest;
    for _ in 0..POLL_ATTEMPTS {
        tokio::time::sleep(POLL_INTERVAL).await;
        let now = pane.state().newest;
        if now == last {
            return now;
        }
        last = now;
    }
    last
}

/// After input, `screen` reproduced through the oracle shows the echoed line.
///
/// # Panics
///
/// When the shell never starts, the command never finishes, or the line is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_round_trips_a_command_through_the_screen() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    // The output `iznik-42-out` differs from the typed `echo iznik-$((6*7))-out`,
    // so finding it proves the command's output round-tripped, not just its echo.
    pane.input(b"echo iznik-$((6*7))-out\n".to_vec())
        .expect("the command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("the command finishes");

    let screen = pane.screen().await.expect("a screen");
    let mut oracle = Vt::new(screen.columns, screen.rows).expect("a fresh oracle");
    oracle.feed(&screen.bytes);
    let text = oracle.screen_text().expect("screen text");
    assert!(
        contains(text.as_bytes(), b"iznik-42-out"),
        "the command's output shows in the reproduction:\n{text}"
    );
}

/// The history is the byte stream: a large `cat` is byte-identical in history
/// from sequence zero, and the published `newest` is its length.
///
/// # Panics
///
/// When the shell never starts, the command never finishes, or the history does
/// not hold the produced bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_history_is_the_byte_stream() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    let content = corpus::generated(0x50a5, 4 * 1024 * 1024);
    let path = std::env::temp_dir().join("iznik-pane-history.bin");
    std::fs::write(&path, &content).expect("the file is written");

    // Raw output, so the file's bytes reach history untranslated by the terminal.
    let command = format!("stty -opost; cat {}\n", path.display());
    pane.input(command.into_bytes())
        .expect("the command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("the cat finishes");
    std::fs::remove_file(&path).expect("the file is removed");

    // Wait for the shell to go idle so history and state see one consistent moment.
    let newest = wait_idle(&pane).await;
    let history = pane.read_history(Sequence(0)).expect("history from zero");
    assert_eq!(
        u64::try_from(history.len()).unwrap_or(u64::MAX),
        newest.0,
        "history from zero is exactly the newest bytes"
    );
    assert!(
        contains(&history, &content),
        "the cat's bytes are byte-identical in history"
    );
}

/// The screen's sequence is the newest at the call, and bytes produced afterward
/// begin at it.
///
/// # Panics
///
/// When the shell never starts, or the sequences do not line up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_sequences_are_exact() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");
    pane.input(b"true\n".to_vec())
        .expect("a command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");

    // Wait until output settles, so the screen and the state agree on the newest.
    let before = wait_idle(&pane).await;
    let screen = pane.screen().await.expect("a screen");
    let after = pane.state().newest;
    assert_eq!(before, after, "the pane is idle across the call");
    assert_eq!(screen.sequence, before, "the screen is exact at the newest");

    pane.input(b"echo tail-marker\n".to_vec())
        .expect("a command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");
    let appended = pane
        .read_history(screen.sequence)
        .expect("history from the screen's sequence");
    assert!(
        contains(&appended, b"tail-marker"),
        "bytes produced afterward begin at the screen's sequence"
    );
}

/// The mirror answers the child's cursor-position query with no subscriber and
/// nothing with one.
///
/// # Panics
///
/// When the shell never starts, or the response policy is not observed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_answers_queries_only_without_subscribers() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    // A probe that reads the terminal's cursor-position reply from its own input,
    // in a non-canonical mode so an unterminated reply is delivered at once.
    let probe = b"probe() { local s; s=$(stty -g); stty -icanon -echo; printf '\\033[6n'; if IFS= read -r -t 1 -d R rep; then r=GOT; else r=NONE; fi; stty \"$s\"; printf '<<%s:%s>>\\n' \"$1\" \"$r\"; }\n";
    pane.input(probe.to_vec()).expect("the probe is defined");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it defines");

    pane.input(b"probe first\n".to_vec())
        .expect("the first probe runs");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");

    let subscription = pane.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;
    pane.input(b"probe second\n".to_vec())
        .expect("the second probe runs");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");
    drop(subscription);

    let history = pane.read_history(Sequence(0)).expect("history from zero");
    assert!(
        contains(&history, b"<<first:GOT>>"),
        "with no subscriber the mirror answered the query"
    );
    assert!(
        contains(&history, b"<<second:NONE>>"),
        "with a subscriber the mirror answered nothing"
    );
}

/// The four OSC 133 marks and the OSC 7 report arrive on `marks` for a command.
///
/// # Panics
///
/// When the shell never starts, or a mark never arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_marks_flow_from_the_shell() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    pane.input(b"cd /tmp && true\n".to_vec())
        .expect("the command is accepted");

    let mut seen_prompt = false;
    let mut seen_command_start = false;
    let mut seen_executed = false;
    let mut seen_finished = false;
    let mut seen_directory = false;
    while !(seen_prompt && seen_command_start && seen_executed && seen_finished && seen_directory) {
        let event = wait_kind(&mut marks, |_kind| true)
            .await
            .expect("a mark arrives");
        match event.kind {
            MarkKind::PromptStart => seen_prompt = true,
            MarkKind::CommandStart => seen_command_start = true,
            MarkKind::CommandExecuted => seen_executed = true,
            MarkKind::CommandFinished { .. } => seen_finished = true,
            MarkKind::WorkingDirectory { .. } => seen_directory = true,
            _other => {}
        }
    }
}

/// The alternate screen is remembered: the reproduction shows the alternate
/// content and reveals the primary on leaving.
///
/// # Panics
///
/// When the shell never starts, or the screens are not remembered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_remembers_the_alternate_screen() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    pane.input(
        b"printf 'PRIMARY-LINE\\n'; printf '\\033[?1049h'; printf 'ALTERNATE-LINE\\n'\n".to_vec(),
    )
    .expect("the command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let screen = pane.screen().await.expect("a screen");
    let mut oracle = Vt::new(screen.columns, screen.rows).expect("a fresh oracle");
    oracle.feed(&screen.bytes);
    assert!(
        oracle.in_alternate_screen().expect("a screen"),
        "the reproduction is on the alternate screen"
    );
    assert!(
        contains(
            oracle.screen_text().expect("screen text").as_bytes(),
            b"ALTERNATE-LINE"
        ),
        "the alternate content shows"
    );
    oracle.feed(b"\x1b[?1049l");
    assert!(
        contains(
            oracle.screen_text().expect("screen text").as_bytes(),
            b"PRIMARY-LINE"
        ),
        "leaving the alternate screen reveals the primary"
    );
}

/// A resize reaches the child and the published state.
///
/// # Panics
///
/// When the shell never starts, or the resize is not observed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_resize_reaches_the_child_and_the_state() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    pane.resize(100, 40).expect("the resize is applied");

    let mut state = pane.state();
    for _ in 0..POLL_ATTEMPTS {
        if (state.columns, state.rows) == (100, 40) {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        state = pane.state();
    }
    assert_eq!(
        (state.columns, state.rows),
        (100, 40),
        "the published state reflects the resize"
    );

    pane.input(b"echo cols:$COLUMNS:\n".to_vec())
        .expect("the command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");
    let history = pane.read_history(Sequence(0)).expect("history from zero");
    assert!(
        contains(&history, b"cols:100:"),
        "the child observed the new width"
    );
}

/// After `close`, the exit status resolves, the state shows exited, and a dropped
/// running pane leaves no process.
///
/// # Panics
///
/// When the shell never starts, the exit is not reported, or a process is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_exits_and_leaves_nothing() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    pane.close().expect("the close signal is sent");
    let status = tokio::time::timeout(DEADLINE, pane.exit_status())
        .await
        .expect("the exit is reported in time");
    assert!(status.is_some(), "an exit status was reported");

    let mut state = pane.state();
    for _ in 0..POLL_ATTEMPTS {
        if state.exited {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        state = pane.state();
    }
    assert!(state.exited, "the state shows the child exited");

    // A pane whose child still runs, dropped, leaves no process behind.
    let running = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("a second pane spawns");
    let mut running_marks = running.marks();
    // A prompt this case asked for, rather than the one the shell printed of
    // its own accord. A broadcast keeps nothing for a receiver that was not
    // yet there, and this shell's first prompt can be printed between the
    // spawn returning and the subscription above being made — which is not a
    // prompt that arrives late but one that is already gone, and a wait for it
    // ends at its deadline. That is how this case failed a release's `claims
    // coverage`: the pane above it had spawned, prompted, closed and exited in
    // twenty-nine milliseconds, and then this prompt did not come in twenty
    // seconds. A newline typed after subscribing is answered with a prompt
    // that cannot have been missed.
    running
        .input(b"\n".to_vec())
        .expect("a newline is accepted");
    wait_kind(&mut running_marks, is_prompt)
        .await
        .expect("its prompt");
    running
        .input(b"sleep 1000\n".to_vec())
        .expect("a long command runs");
    wait_kind(&mut running_marks, |kind| {
        matches!(kind, MarkKind::CommandExecuted)
    })
    .await
    .expect("the sleep is running");
    let pid = running.process_id();
    assert!(process_exists(pid), "the child is running before the drop");
    drop(running);
    drop(running_marks);
    assert!(
        wait_until_gone(pid).await,
        "dropping the pane left no process for pid {pid}"
    );
}

/// Whether a process with `pid` still exists, by its `/proc` entry.
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Whether `pid` is gone within the poll budget, polling its `/proc` entry.
async fn wait_until_gone(pid: u32) -> bool {
    for _ in 0..POLL_ATTEMPTS {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    !process_exists(pid)
}

/// Many panes sharing one mirror thread — the production configuration — each
/// round-trips a command whose output the typed line cannot contain, so a mark
/// or output that never propagates on the shared thread times out.
///
/// # Panics
///
/// When any pane's prompt or command never arrives, or its output is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_many_share_one_thread() {
    const PANES: usize = 6;
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let mut panes = Vec::new();
    for _ in 0..PANES {
        let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
            .await
            .expect("a pane spawns");
        let marks = pane.marks();
        panes.push((pane, marks));
    }
    for (index, (pane, marks)) in panes.iter_mut().enumerate() {
        wait_kind(marks, is_prompt).await.expect("a prompt");
        pane.input(format!("echo $((6*7))-{index}\n").into_bytes())
            .expect("a command is accepted");
        wait_kind(marks, is_finished).await.expect("it finishes");
        let history = pane.read_history(Sequence(0)).expect("history from zero");
        assert!(
            contains(&history, format!("42-{index}").as_bytes()),
            "pane {index}'s output round-trips on the shared thread"
        );
    }
}

/// Repeated alternate-screen switches in one command — enter, leave, enter —
/// are remembered in order: the reproduction is the last alternate content, and
/// leaving reveals the primary as it stood at the last enter (with `PRIMARY-TWO`),
/// not a primary lost by a mis-ordered snapshot.
///
/// # Panics
///
/// When the shell never starts, or the wrong primary or alternate is remembered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_remembers_repeated_alternate_switches() {
    let thread = MirrorThread::start().expect("the mirror thread starts");
    let pane = Pane::spawn(&shell_options(COLUMNS, ROWS), HISTORY_BYTES, &thread)
        .await
        .expect("the pane spawns");
    let mut marks = pane.marks();
    wait_kind(&mut marks, is_prompt)
        .await
        .expect("the first prompt");

    // One printf: primary, enter, alt, leave, primary, enter, alt — ending on the
    // alternate screen after two enters and one leave.
    pane.input(
        b"printf 'PRIMARY-ONE\\n\\033[?1049hALT-ONE\\n\\033[?1049lPRIMARY-TWO\\n\\033[?1049hALT-TWO\\n'\n"
            .to_vec(),
    )
    .expect("the command is accepted");
    wait_kind(&mut marks, is_finished)
        .await
        .expect("it finishes");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let screen = pane.screen().await.expect("a screen");
    let mut oracle = Vt::new(screen.columns, screen.rows).expect("a fresh oracle");
    oracle.feed(&screen.bytes);
    assert!(
        oracle.in_alternate_screen().expect("a screen"),
        "the reproduction ends on the alternate screen"
    );
    let alternate = oracle.screen_text().expect("screen text");
    assert!(
        contains(alternate.as_bytes(), b"ALT-TWO"),
        "the last alternate content shows"
    );
    assert!(
        !contains(alternate.as_bytes(), b"ALT-ONE"),
        "the first alternate, cleared on re-enter, does not"
    );

    oracle.feed(b"\x1b[?1049l");
    let primary = oracle.screen_text().expect("screen text");
    assert!(
        contains(primary.as_bytes(), b"PRIMARY-TWO"),
        "leaving reveals the primary remembered at the last enter, not a lost one"
    );
}
