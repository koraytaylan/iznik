//! Shared foreground job identity, failure cleanup and bounded process census.

use std::time::Duration;

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::{Pid, getpgid, getsid};

/// Longer than any test deadline; only owner cleanup should end the fixture.
const SLEEP_SECONDS: u32 = 1000;
/// A normal teardown takes milliseconds; two seconds exposes a missing kill promptly.
const CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
/// Short polling yields to the child reaper without slowing an in-process proof.
const CLEANUP_INTERVAL: Duration = Duration::from_millis(5);

/// Time budgets for observing kernel process removal.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CleanupOptions {
    /// Maximum time allowed for the kernel and child reaper to remove the process.
    deadline: Duration,
    /// Yield between process census checks.
    interval: Duration,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            deadline: CLEANUP_DEADLINE,
            interval: CLEANUP_INTERVAL,
        }
    }
}

/// The same announced foreground fixture command in both PTY and pane proofs.
pub(crate) fn command() -> String {
    format!(
        "sh '{}/tests/fixtures/foreground.sh' {SLEEP_SECONDS}",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Whether the kernel still exposes this process, including an unreaped
/// zombie: signal zero asks without sending anything, where a `/proc` path
/// would not exist on macOS and would say every process was gone.
pub(crate) fn process_exists(process: u32) -> bool {
    i32::try_from(process)
        .map(Pid::from_raw)
        .is_ok_and(|id| kill(id, None).is_ok())
}

/// Wait for actual removal rather than merely observing that a signal was sent.
pub(crate) async fn wait_until_gone(process: u32, options: CleanupOptions) -> bool {
    wait_until(options, || !process_exists(process)).await
}

/// Poll one lifecycle fact under the same short budget as process removal.
pub(crate) async fn wait_until(options: CleanupOptions, mut ready: impl FnMut() -> bool) -> bool {
    let started = tokio::time::Instant::now();
    while !ready() && started.elapsed() < options.deadline {
        tokio::time::sleep(
            options
                .interval
                .min(options.deadline.saturating_sub(started.elapsed())),
        )
        .await;
    }
    ready()
}

/// A verified fixture job, killed on a failing test only while still in its owned session.
#[derive(Debug)]
pub(crate) struct ForegroundJob {
    /// PID announced by the foreground child, also its process-group identity.
    pub(crate) process: u32,
    /// Shell session used to reject a stale or unrelated identity during cleanup.
    session: Pid,
}

impl ForegroundJob {
    /// Verify the announced child belongs to the terminal session before retaining it.
    ///
    /// # Errors
    /// Returns invalid PID conversions or a kernel/session mismatch.
    pub(crate) fn new(process: u32, session: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let group = Pid::from_raw(i32::try_from(process)?);
        let session = Pid::from_raw(i32::try_from(session)?);
        if group.as_raw() <= 0
            || group == session
            || getpgid(Some(group))? != group
            || getsid(Some(group))? != session
        {
            return Err("foreground child is not a distinct group in the owned session".into());
        }
        Ok(Self { process, session })
    }
}

impl Drop for ForegroundJob {
    fn drop(&mut self) {
        let Ok(process) = i32::try_from(self.process) else {
            return;
        };
        let group = Pid::from_raw(process);
        if getsid(Some(group)) == Ok(self.session) {
            let _killed = killpg(group, Signal::SIGKILL);
        }
    }
}
