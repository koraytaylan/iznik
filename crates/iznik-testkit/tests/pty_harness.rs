//! The pseudoterminal harness: an echo round trip through `sh`, quiet that
//! is silence and not a clock, a cap that is never an empty success, a
//! resize the child observes, exit statuses that never fake a code, and a
//! drop that leaves no process behind.

use std::time::{Duration, Instant};

use iznik_testkit::pty::{ExitStatus, PtyChild, PtyError};

/// Silence that ends a read: longer than any pause a child here takes
/// between two writes, shorter than a second by far.
const QUIET: Duration = Duration::from_millis(150);

/// The most a read waits, well under a second.
const CAP: Duration = Duration::from_millis(800);

/// The width every child here starts at.
const COLUMNS: u16 = 80;

/// The height every child here starts at.
const ROWS: u16 = 24;

/// `sh` spawned with `PS1='$ '` echoes a written line, and the read returns
/// the echo and the prompt and nothing else.
///
/// The first read is the prompt alone on Linux and a short line of the
/// platform's own on macOS, where an interactive `sh` prints a notice before
/// `PS1`; what this case holds is that the prompt is there and that it is
/// followed by exactly the echo and the answer.
///
/// # Panics
///
/// When the prompt, the echo or the answer differ from the bytes a
/// terminal shows for them.
#[test]
fn pty_harness_echo_round_trips_through_sh() {
    // `ENV` is blanked so an interactive `sh` sources nothing of the
    // developer's; the prompt is then exactly what `PS1` says.
    let mut child =
        PtyChild::spawn("env", &["ENV=", "PS1=$ ", "sh", "-i"], COLUMNS, ROWS).expect("sh spawns");
    let prompt = child
        .read_until_quiet(QUIET, CAP)
        .expect("the prompt arrives");
    let prompt_text = String::from_utf8_lossy(&prompt);
    assert!(
        prompt_text.contains("$ ") || prompt_text.contains("sh-"),
        "the prompt arrives: {prompt_text:?}"
    );
    child.write(b"echo hi\n").expect("the line is written");
    let answer = child
        .read_until_quiet(QUIET, CAP)
        .expect("the echo arrives");
    assert_eq!(
        String::from_utf8_lossy(&answer),
        "echo hi\r\nhi\r\n$ ",
        "the echo, the output and the next prompt, and nothing else"
    );
    child.write(b"exit\n").expect("exit is written");
    assert_eq!(child.wait().expect("sh ends"), ExitStatus::Exited(0));
}

/// A child that prints, pauses briefly, then prints more is read as one
/// result when `quiet` exceeds the pause and as two when it does not.
///
/// # Panics
///
/// When the long quiet does not join the two prints or the short quiet does
/// not split them.
#[test]
fn pty_harness_quiet_is_silence_not_a_clock() {
    let script = "printf a; sleep 0.2; printf b";
    let mut joined = PtyChild::spawn("sh", &["-c", script], COLUMNS, ROWS).expect("sh spawns");
    let both = joined
        .read_until_quiet(Duration::from_millis(400), CAP)
        .expect("one result");
    assert_eq!(both, b"ab");
    let mut split = PtyChild::spawn("sh", &["-c", script], COLUMNS, ROWS).expect("sh spawns");
    let first = split
        .read_until_quiet(Duration::from_millis(50), CAP)
        .expect("the first print");
    assert_eq!(first, b"a");
    let second = split
        .read_until_quiet(Duration::from_millis(50), CAP)
        .expect("the second print");
    assert_eq!(second, b"b");
}

/// A child that never prints produces `Timeout` at the cap carrying the
/// escaped bytes received so far, never an empty success.
///
/// # Panics
///
/// When the read succeeds, ends early, or reports bytes it did not see.
#[test]
fn pty_harness_the_cap_is_never_an_empty_success() {
    let cap = Duration::from_millis(200);
    let mut child = PtyChild::spawn("sleep", &["5"], COLUMNS, ROWS).expect("sleep spawns");
    let started = Instant::now();
    let outcome = child.read_until_quiet(QUIET, cap);
    let elapsed = started.elapsed();
    assert!(
        matches!(&outcome, Err(PtyError::Timeout { received }) if received.is_empty()),
        "{outcome:?}"
    );
    assert!(
        elapsed >= cap,
        "the read ended at {elapsed:?}, before the cap"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "the read took {elapsed:?}"
    );
}

/// A child that closes its terminal having produced nothing is `Closed`,
/// never an empty success.
///
/// # Panics
///
/// When the read succeeds or reports anything but `Closed`.
#[test]
fn pty_harness_a_silent_exit_is_closed_not_an_empty_success() {
    let mut child = PtyChild::spawn("sh", &["-c", "exit 0"], COLUMNS, ROWS).expect("sh spawns");
    let outcome = child.read_until_quiet(QUIET, CAP);
    assert!(
        matches!(&outcome, Err(PtyError::Closed { received }) if received.is_empty()),
        "{outcome:?}"
    );
}

/// A `sh` script that traps `WINCH` and prints `stty size` reports the new
/// columns and rows after `resize`.
///
/// # Panics
///
/// When the first or the second size line is not what the terminal was
/// given.
#[test]
fn pty_harness_resize_is_observed_by_the_child() {
    let script = "trap 'stty size' WINCH; stty size; while :; do sleep 0.05; done";
    let mut child = PtyChild::spawn("sh", &["-c", script], COLUMNS, ROWS).expect("sh spawns");
    let initial = child.read_until_quiet(QUIET, CAP).expect("the first size");
    assert_eq!(String::from_utf8_lossy(&initial), "24 80\r\n");
    child.resize(100, 40).expect("resizes");
    let resized = child.read_until_quiet(QUIET, CAP).expect("the second size");
    assert_eq!(String::from_utf8_lossy(&resized), "40 100\r\n");
}

/// A child that exits with a code reports `Exited(code)`; one killed by a
/// signal reports `Signalled` with the signal's name, never a fake code.
///
/// # Panics
///
/// When either status is not as described.
#[test]
fn pty_harness_exit_statuses_never_fake_a_code() {
    let coded = PtyChild::spawn("sh", &["-c", "exit 3"], COLUMNS, ROWS).expect("sh spawns");
    assert_eq!(coded.wait().expect("sh ends"), ExitStatus::Exited(3));
    let signalled =
        PtyChild::spawn("sh", &["-c", "kill -TERM $$"], COLUMNS, ROWS).expect("sh spawns");
    let status = signalled.wait().expect("sh ends");
    assert_eq!(status, ExitStatus::Signalled("Terminated".to_owned()));
}

/// A `PtyChild` dropped while its child runs leaves no process behind.
///
/// # Panics
///
/// When the child's process still exists after the drop.
#[test]
fn pty_harness_drop_leaves_no_process_behind() {
    let child = PtyChild::spawn("sleep", &["30"], COLUMNS, ROWS).expect("sleep spawns");
    let process_id = child.process_id();
    assert!(process_exists(process_id), "the child runs");
    drop(child);
    assert!(!process_exists(process_id), "the child survived the drop");
}

/// Whether a process id still names a process, on every platform this runs
/// on: signal zero asks the kernel without sending anything, where a `/proc`
/// path would not exist on macOS and would say every process was gone.
fn process_exists(process_id: u32) -> bool {
    let Ok(signed) = i32::try_from(process_id) else {
        return false;
    };
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(signed), None).is_ok()
}
