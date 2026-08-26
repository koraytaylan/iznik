//! The deadline helpers: a capped body returns its value or `Elapsed` with
//! its context, a body without a value is `Abandoned`, and a polled condition
//! returns as soon as it holds or `Elapsed` at the cap. Every deadline here is
//! well under a second.

use std::cell::Cell;
use std::thread;
use std::time::{Duration, Instant};

use iznik_harness::deadline::{DeadlineError, run_capped, wait_until};

/// A cap short enough that a test spends no time on it and long enough that
/// a scheduler hiccup does not fire it.
const CAP: Duration = Duration::from_millis(200);

/// How much later than its cap a wait may return before the bound is not a
/// bound: scheduler and thread start-up jitter, generously.
const SLACK: Duration = Duration::from_millis(150);

/// The polling interval of the condition waits.
const INTERVAL: Duration = Duration::from_millis(5);

/// A body that finishes in time returns its value.
///
/// # Panics
///
/// When the value does not come back.
#[test]
fn deadline_run_capped_returns_the_value() {
    let value = run_capped(CAP, "a prompt body", || "done").expect("the body finishes in time");
    assert_eq!(value, "done", "the body's value");
}

/// A body that outlives its cap is `Elapsed` with its context, within the cap
/// plus a stated slack.
///
/// # Panics
///
/// When the wait does not give up in time or the error is another.
#[test]
fn deadline_run_capped_elapses_with_its_context() {
    let started = Instant::now();
    let result = run_capped(CAP, "a body that sleeps", || {
        thread::sleep(CAP.saturating_mul(2));
    });
    let waited = started.elapsed();
    assert_eq!(
        result,
        Err(DeadlineError::Elapsed {
            cap: CAP,
            context: "a body that sleeps".to_owned(),
        }),
        "the error"
    );
    assert!(waited >= CAP, "gave up before the cap: {waited:?}");
    assert!(
        waited < CAP.saturating_add(SLACK),
        "gave up late: {waited:?}"
    );
}

/// A body that ends without a value — it panicked — is `Abandoned` with its
/// context, not `Elapsed` and not a hang.
///
/// # Panics
///
/// When the error is another.
#[test]
fn deadline_run_capped_reports_a_body_without_a_value() {
    let result = run_capped(CAP, "a body that panics", || -> u32 {
        panic!("the body panics on purpose")
    });
    let Err(DeadlineError::Abandoned { context, reason }) = result else {
        panic!("expected the body to be abandoned, got {result:?}");
    };
    assert_eq!(context, "a body that panics", "the context");
    assert!(
        reason.contains("without a value"),
        "the reason says why: {reason}"
    );
}

/// A condition that holds at once returns at once.
///
/// # Panics
///
/// When the wait fails or takes longer than one interval.
#[test]
fn deadline_wait_until_returns_when_the_condition_holds() {
    let started = Instant::now();
    wait_until(CAP, INTERVAL, "an immediate condition", || true).expect("holds at once");
    assert!(
        started.elapsed() < INTERVAL.saturating_add(SLACK),
        "returned late: {:?}",
        started.elapsed()
    );
}

/// A condition that comes to hold after a few polls returns as soon as it
/// does, not at the cap.
///
/// # Panics
///
/// When the wait fails, polls the wrong number of times, or returns late.
#[test]
fn deadline_wait_until_returns_as_soon_as_the_condition_holds() {
    let polls = Cell::new(0_u32);
    let started = Instant::now();
    wait_until(CAP, INTERVAL, "the third poll", || {
        polls.set(polls.get().saturating_add(1));
        polls.get() == 3
    })
    .expect("holds on the third poll");
    assert_eq!(polls.get(), 3, "polls until it held");
    assert!(
        started.elapsed() < CAP,
        "returned at the cap rather than when it held: {:?}",
        started.elapsed()
    );
}

/// A condition that never holds is `Elapsed` naming the context, at the cap.
///
/// # Panics
///
/// When the wait succeeds, gives up early, or gives up late.
#[test]
fn deadline_wait_until_elapses_at_the_cap() {
    let started = Instant::now();
    let result = wait_until(CAP, INTERVAL, "a condition that never holds", || false);
    let waited = started.elapsed();
    assert_eq!(
        result,
        Err(DeadlineError::Elapsed {
            cap: CAP,
            context: "a condition that never holds".to_owned(),
        }),
        "the error"
    );
    assert!(waited >= CAP, "gave up before the cap: {waited:?}");
    assert!(
        waited < CAP.saturating_add(SLACK),
        "gave up late: {waited:?}"
    );
}

/// An interval longer than the cap never sleeps past the cap.
///
/// # Panics
///
/// When the wait returns later than the cap plus the slack.
#[test]
fn deadline_wait_until_never_sleeps_past_the_cap() {
    let started = Instant::now();
    let result = wait_until(CAP, CAP.saturating_mul(10), "a long interval", || false);
    let waited = started.elapsed();
    assert!(result.is_err(), "the condition never held");
    assert!(
        waited < CAP.saturating_add(SLACK),
        "slept past the cap: {waited:?}"
    );
}
