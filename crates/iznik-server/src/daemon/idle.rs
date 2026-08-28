//! The idle interval after which a daemon with no panes and no clients exits.
//!
//! A daemon lingering for ever on a shared host with nothing to do is rude,
//! and one that exits while it holds a pane has thrown away a session. So the
//! rule is both: no panes *and* no clients, for the whole interval. Every
//! timing is a parameter — [`IDLE_SHUTDOWN`] is only the default — so no test
//! ever waits ten minutes to see it.

use core::time::Duration;

/// How long a daemon with no panes and no clients waits before exiting.
pub const IDLE_SHUTDOWN: Duration = Duration::from_mins(10);

/// How long a daemon that has been asked to start waits for its socket to
/// answer before saying it did not.
pub const SOCKET_READY_CAP: Duration = Duration::from_secs(10);

/// How long `--stop` waits for a daemon's socket to disappear.
pub const STOP_CAP: Duration = Duration::from_secs(10);

/// How long the accept loop waits before looking at whether it is idle, so an
/// idle daemon costs one wake a second rather than a spin.
pub const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// How long a command waiting for a socket to appear or go waits between
/// looks. Short, because it is a person or a bootstrap waiting on it.
pub const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a daemon has had nothing to do.
#[derive(Clone, Copy, Debug)]
pub struct Idle {
    /// When it last had a pane or a client, or `None` while it has one.
    since: Option<std::time::Instant>,
}

impl Idle {
    /// A daemon that has just started, and so has been idle since now.
    #[must_use]
    pub fn new() -> Idle {
        Idle {
            since: Some(std::time::Instant::now()),
        }
    }

    /// Records what the daemon holds now.
    pub fn observe(&mut self, panes: usize, clients: usize) {
        if panes > 0 || clients > 0 {
            self.since = None;
        } else if self.since.is_none() {
            self.since = Some(std::time::Instant::now());
        }
    }

    /// Whether it has had nothing to do for `interval`.
    #[must_use]
    pub fn expired(&self, interval: Duration) -> bool {
        self.since.is_some_and(|since| since.elapsed() >= interval)
    }
}

impl Default for Idle {
    fn default() -> Idle {
        Idle::new()
    }
}
