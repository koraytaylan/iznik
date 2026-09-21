//! The bounded process runner: a prompt child completes with its output and
//! elapsed time; a child past its deadline is ended with its whole process
//! group and reported with what it said; output streams through under
//! `Inherit`, is capped with its tail kept under `Capture`, and is held whole
//! under `Whole`; a program that cannot start and a child that fails are each
//! named.
//!
//! Children here are `sh` and coreutils, never a login shell. Every deadline
//! is under a second; the one case that waits out the termination grace is
//! the child that ignores `SIGTERM`, and the grace is a product constant.

use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iznik_harness::deadline::wait_until;
use iznik_harness::process::{
    CAPTURE_LIMIT_BYTES, Completed, Deadline, Output, ProcessError, TERMINATION_GRACE, run,
};

/// A deadline short enough that a test spends no time on it and long enough
/// that a prompt child never trips it.
const DEADLINE: Duration = Duration::from_millis(300);

/// How much later than its deadline a timed-out run may return: the poll
/// interval, signal delivery and thread scheduling, generously.
const SLACK: Duration = Duration::from_millis(300);

/// How long the census is given to see a killed group disappear: signal
/// delivery is asynchronous, though it takes microseconds.
const CENSUS_CAP: Duration = Duration::from_millis(500);

/// How often the census looks again.
const CENSUS_INTERVAL: Duration = Duration::from_millis(10);

/// The state letter of a process that has exited and awaits its parent's
/// reaping; such a process is not a survivor.
const ZOMBIE_STATE: &str = "Z";

/// The variable that turns the streaming helper test into the child of the
/// streaming test; unset, the helper does nothing.
const STREAMING_HELPER_VARIABLE: &str = "IZNIK_PROCESS_STREAMING_HELPER";

/// The pause the streaming helper's child makes between its two lines, as a
/// shell argument; long enough to tell streaming from buffering.
const STREAMING_PAUSE_SECONDS: &str = "0.3";

/// The least gap the streaming test accepts between its two lines: the pause
/// minus scheduling jitter.
const STREAMING_MINIMUM_GAP: Duration = Duration::from_millis(200);

/// The time the streaming helper process gets before this test kills it.
const STREAMING_HELPER_DEADLINE: Duration = Duration::from_secs(10);

/// A shell command line as a `Command`.
fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script);
    command
}

/// The process ids of the live members of the process group `group`, asked of
/// `ps`; a zombie is not a survivor.
///
/// `ps` rather than `/proc`: the fields are the same on every platform this
/// runs on, where a `/proc` listing exists only on Linux.
///
/// # Errors
///
/// When `ps` cannot be run or does not answer.
fn process_group_members(group: u32) -> Result<Vec<u32>, String> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .output()
        .map_err(|error| format!("running ps: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ps failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let mut members = Vec::new();
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let Some(process_id) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
            continue;
        };
        let group_of = fields.next().and_then(|field| field.parse::<u32>().ok());
        let state = fields.next().unwrap_or_default();
        if group_of == Some(group) && !state.starts_with(ZOMBIE_STATE) {
            members.push(process_id);
        }
    }
    Ok(members)
}

/// Asserts, within the census cap, that no live process of the group remains.
///
/// # Errors
///
/// When the census cannot run or the group still has members at the cap.
fn assert_group_gone(group: u32) -> Result<(), String> {
    wait_until(
        CENSUS_CAP,
        CENSUS_INTERVAL,
        "the process group to end",
        || process_group_members(group).is_ok_and(|members| members.is_empty()),
    )
    .map_err(|error| format!("{error}: {:?}", process_group_members(group)))
}

/// A child that exits promptly completes with its status, its output and its
/// elapsed time.
///
/// # Panics
///
/// When the run fails or reports something other than what the child did.
#[test]
fn process_prompt_child_completes() {
    let completed: Completed = run(
        shell("printf out; printf err >&2"),
        Deadline(DEADLINE),
        Output::Capture,
    )
    .expect("the child completes");
    assert!(completed.status.success(), "the status");
    assert_eq!(completed.stdout, b"out", "stdout");
    assert_eq!(completed.stderr, b"err", "stderr");
    assert!(
        completed.elapsed < DEADLINE,
        "elapsed: {:?}",
        completed.elapsed
    );
}

/// A child past its deadline is `TimedOut` within the deadline plus the
/// slack, carrying what it said, and neither it nor its grandchild survives.
///
/// # Panics
///
/// When the run does not time out as described or a process survives.
#[test]
fn process_child_past_its_deadline_is_ended_with_its_group() {
    let started = Instant::now();
    let result = run(
        shell("echo started; sleep 30 & exec sleep 30"),
        Deadline(DEADLINE),
        Output::Capture,
    );
    let waited = started.elapsed();
    let Err(ProcessError::TimedOut {
        program,
        deadline,
        stdout_tail,
        ..
    }) = result
    else {
        panic!("expected a timeout, got {result:?}");
    };
    assert_eq!(program, "sh", "the program");
    assert_eq!(deadline, DEADLINE, "the deadline");
    assert_eq!(stdout_tail, "started\n", "what the child said");
    assert!(waited >= DEADLINE, "ended before the deadline: {waited:?}");
    assert!(
        waited < DEADLINE.saturating_add(SLACK),
        "ended late: {waited:?}"
    );
}

/// A census of the group after a timeout finds nobody: the process group id
/// is the child's own process id, learned from the child itself.
///
/// # Panics
///
/// When the run does not time out or a member of the group survives.
#[test]
fn process_group_does_not_survive_a_timeout() {
    let result = run(
        shell("echo $$; sleep 30 & sleep 30 & exec sleep 30"),
        Deadline(DEADLINE),
        Output::Capture,
    );
    let Err(ProcessError::TimedOut { stdout_tail, .. }) = result else {
        panic!("expected a timeout, got {result:?}");
    };
    let group: u32 = stdout_tail
        .trim()
        .parse()
        .expect("the child printed its process id");
    assert_group_gone(group).expect("the group is gone");
}

/// A child that ignores `SIGTERM` is sent `SIGKILL` after the grace, and is
/// reported within the deadline plus the grace plus the slack.
///
/// # Panics
///
/// When the run does not time out, or the group survives the kill.
#[test]
fn process_child_ignoring_sigterm_is_killed_after_the_grace() {
    let started = Instant::now();
    let result = run(
        shell("trap '' TERM; echo $$; exec sleep 30"),
        Deadline(DEADLINE),
        Output::Capture,
    );
    let waited = started.elapsed();
    let Err(ProcessError::TimedOut { stdout_tail, .. }) = result else {
        panic!("expected a timeout, got {result:?}");
    };
    let group: u32 = stdout_tail
        .trim()
        .parse()
        .expect("the child printed its process id");
    assert!(
        waited >= DEADLINE.saturating_add(TERMINATION_GRACE),
        "killed before the grace: {waited:?}"
    );
    assert!(
        waited
            < DEADLINE
                .saturating_add(TERMINATION_GRACE)
                .saturating_add(SLACK),
        "killed late: {waited:?}"
    );
    assert_group_gone(group).expect("the group is gone");
}

/// Under `Whole`, a flood past the cap is held in its entirety, with nothing
/// trimmed and nothing said about dropping — which is what lets a caller
/// parse a document larger than a capture may keep.
///
/// # Panics
///
/// When any byte is missing.
#[test]
fn process_whole_output_holds_a_flood_past_the_cap() {
    let flood = CAPTURE_LIMIT_BYTES.saturating_mul(3).saturating_div(2);
    let completed = run(
        shell(&format!(
            "printf first; head -c {flood} /dev/zero | tr '\\0' a; printf last"
        )),
        Deadline(Duration::from_secs(5)),
        Output::Whole,
    )
    .expect("the flood completes");
    assert!(
        completed.stdout.starts_with(b"first"),
        "the first bytes are there"
    );
    assert!(
        completed.stdout.ends_with(b"last"),
        "the last bytes are there"
    );
    assert_eq!(
        completed.stdout.len(),
        "firstlast".len().saturating_add(flood),
        "every byte is there"
    );
    assert!(
        !String::from_utf8_lossy(&completed.stdout).contains("bytes dropped"),
        "nothing is said to have been dropped"
    );
}

/// Under `Capture`, output beyond the cap is truncated with the tail kept and
/// the truncation stated.
///
/// # Panics
///
/// When the capture is not capped, loses its tail, or does not say so.
#[test]
fn process_capture_keeps_the_tail_and_states_the_truncation() {
    let flood = CAPTURE_LIMIT_BYTES.saturating_mul(3).saturating_div(2);
    let completed = run(
        shell(&format!(
            "head -c {flood} /dev/zero | tr '\\0' a; printf END"
        )),
        Deadline(Duration::from_secs(5)),
        Output::Capture,
    )
    .expect("the flood completes");
    let stdout = String::from_utf8_lossy(&completed.stdout);
    assert!(stdout.ends_with("END"), "the tail is kept");
    let statement = stdout.lines().next().expect("a first line");
    assert!(
        statement.starts_with('[') && statement.contains("bytes dropped"),
        "the truncation is stated: {statement}"
    );
    assert!(
        completed.stdout.len()
            <= CAPTURE_LIMIT_BYTES
                .saturating_add(statement.len())
                .saturating_add(1),
        "the capture is capped: {} bytes",
        completed.stdout.len()
    );
}

/// A stream a grandchild keeps open after the child exits is drained for the
/// grace and then taken with the incompleteness stated, so the caller is
/// neither held nor misled.
///
/// # Panics
///
/// When the run is held by the grandchild or the statement is missing.
#[test]
fn process_capture_states_a_stream_left_open() {
    let started = Instant::now();
    let completed = run(
        shell("echo said; sleep 5 & exit 0"),
        Deadline(Duration::from_secs(5)),
        Output::Capture,
    )
    .expect("the child completes");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the grandchild held the run: {:?}",
        started.elapsed()
    );
    let stdout = String::from_utf8_lossy(&completed.stdout);
    assert!(
        stdout.starts_with("said\n"),
        "what the child said: {stdout}"
    );
    assert!(
        stdout.contains("still open after the child exited"),
        "the open stream is stated: {stdout}"
    );
}

/// A program that cannot be spawned is `Spawn` naming it.
///
/// # Panics
///
/// When the run returns anything else.
#[test]
fn process_program_that_cannot_start_is_spawn() {
    let result = run(
        Command::new("/nonexistent/iznik-program"),
        Deadline(DEADLINE),
        Output::Capture,
    );
    let Err(ProcessError::Spawn { program, .. }) = result else {
        panic!("expected a spawn error, got {result:?}");
    };
    assert_eq!(program, "/nonexistent/iznik-program", "the program");
}

/// A child exiting non-zero is `Failed` with its status and its stderr tail.
///
/// # Panics
///
/// When the run returns anything else.
#[test]
fn process_child_exiting_nonzero_is_failed() {
    let result = run(
        shell("echo oops >&2; exit 3"),
        Deadline(DEADLINE),
        Output::Capture,
    );
    let Err(ProcessError::Failed {
        program,
        status,
        stderr_tail,
    }) = result
    else {
        panic!("expected a failure, got {result:?}");
    };
    assert_eq!(program, "sh", "the program");
    assert_eq!(status.code(), Some(3), "the status");
    assert_eq!(stderr_tail, "oops\n", "the stderr tail");
}

/// The child half of the streaming case: under the helper variable, runs a
/// child that prints, pauses, then prints again, with its output inherited —
/// so whoever spawned this process sees each line as it is printed. Without
/// the variable it does nothing, which is what a helper does when it is not
/// asked.
///
/// # Panics
///
/// When the inherited run fails.
#[test]
fn process_streaming_helper() {
    if env::var_os(STREAMING_HELPER_VARIABLE).is_none() {
        return;
    }
    run(
        shell(&format!(
            "echo first; sleep {STREAMING_PAUSE_SECONDS}; echo second"
        )),
        Deadline(Duration::from_secs(2)),
        Output::Inherit,
    )
    .expect("the inherited child completes");
}

/// Under `Inherit`, a child's output reaches the parent's streams before the
/// child exits: this test binary is run as a child on the helper case above,
/// with its standard output piped here, and the time between its two lines
/// is the pause the child made — not zero, which is what buffering to the
/// end would show.
///
/// The helper is bounded by a watchdog of this test's own rather than by
/// coreutils `timeout`, which does not exist on every platform this runs on.
///
/// # Panics
///
/// When the helper cannot be run, a line is missing, or the lines arrive
/// together.
#[test]
fn process_inherit_streams_output_as_it_happens() {
    let this_binary = env::current_exe().expect("this test binary");
    let mut helper = Command::new(this_binary)
        .args(["process_streaming_helper", "--exact", "--nocapture"])
        .env(STREAMING_HELPER_VARIABLE, "1")
        .stdout(Stdio::piped())
        .spawn()
        .expect("the helper starts");
    let watchdog = helper.id();
    let bounded = Arc::new(AtomicBool::new(false));
    let watching = Arc::clone(&bounded);
    let guard = std::thread::spawn(move || {
        let until = Instant::now() + STREAMING_HELPER_DEADLINE;
        while Instant::now() < until {
            if watching.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _killed = Command::new("kill")
            .args(["-9", &watchdog.to_string()])
            .status();
    });
    let stdout = helper.stdout.take().expect("the helper's stdout");
    let started = Instant::now();
    let mut first = None;
    let mut second = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("a line from the helper");
        match line.as_str() {
            "first" => first = Some(started.elapsed()),
            "second" => second = Some(started.elapsed()),
            _ => {}
        }
    }
    bounded.store(true, Ordering::Release);
    let status = helper.wait().expect("the helper exits");
    guard.join().expect("the watchdog ends");
    assert!(status.success(), "the helper passed: {status}");
    let first = first.expect("the first line arrived");
    let second = second.expect("the second line arrived");
    assert!(
        second.saturating_sub(first) >= STREAMING_MINIMUM_GAP,
        "the lines arrived together, at {first:?} and {second:?}"
    );
}
