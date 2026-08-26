//! The two deadline helpers every fixture wait is written with: a capped body
//! and a polled condition, each naming what it waited for. A hang is a
//! failure that says what was running, never a wait.

use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// Why a wait did not end with its value.
#[derive(Debug, PartialEq, Eq)]
pub enum DeadlineError {
    /// The cap passed before the body finished or the condition held.
    Elapsed {
        /// The cap that passed.
        cap: Duration,
        /// What was being waited for.
        context: String,
    },
    /// The body did not produce a value: its thread could not be started,
    /// or it ended without one, which only a panic does.
    Abandoned {
        /// What was being waited for.
        context: String,
        /// Why there is no value.
        reason: String,
    },
}

impl Display for DeadlineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            DeadlineError::Elapsed { cap, context } => {
                write!(formatter, "{context}: not done within {cap:?}")
            }
            DeadlineError::Abandoned { context, reason } => {
                write!(formatter, "{context}: {reason}")
            }
        }
    }
}

impl std::error::Error for DeadlineError {}

/// Runs `body` on a thread and waits at most `cap` for its value.
///
/// The thread is not ended when the cap passes — a thread cannot be — so a
/// body that owns something that outlives it, such as containers, registers
/// that something with a reaper as well; this is a bound on the wait, not on
/// the work.
///
/// # Errors
///
/// [`DeadlineError::Elapsed`] when the cap passes first, and
/// [`DeadlineError::Abandoned`] when the body's thread cannot be started or
/// the body ends without a value.
pub fn run_capped<Value: Send + 'static>(
    cap: Duration,
    context: &str,
    body: impl FnOnce() -> Value + Send + 'static,
) -> Result<Value, DeadlineError> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("iznik-capped".to_owned())
        .spawn(move || {
            let value = body();
            sender.send(value).unwrap_or_default();
        })
        .map_err(|error| DeadlineError::Abandoned {
            context: context.to_owned(),
            reason: format!("the body's thread could not be started: {error}"),
        })?;
    match receiver.recv_timeout(cap) {
        Ok(value) => Ok(value),
        Err(RecvTimeoutError::Timeout) => Err(DeadlineError::Elapsed {
            cap,
            context: context.to_owned(),
        }),
        Err(RecvTimeoutError::Disconnected) => Err(DeadlineError::Abandoned {
            context: context.to_owned(),
            reason: "the body ended without a value".to_owned(),
        }),
    }
}

/// Polls `condition` every `interval` until it holds, for at most `cap`.
///
/// The condition is checked before the first sleep and once more as the cap
/// passes, and a sleep never runs past the cap.
///
/// # Errors
///
/// [`DeadlineError::Elapsed`] when the cap passes with the condition false.
pub fn wait_until(
    cap: Duration,
    interval: Duration,
    context: &str,
    mut condition: impl FnMut() -> bool,
) -> Result<(), DeadlineError> {
    let started = Instant::now();
    loop {
        if condition() {
            return Ok(());
        }
        let remaining = cap.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DeadlineError::Elapsed {
                cap,
                context: context.to_owned(),
            });
        }
        thread::sleep(interval.min(remaining));
    }
}
