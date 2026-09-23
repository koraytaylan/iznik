//! Marks, sizes and exits pulled into the model, plus a foreground-program
//! sample when the daemon is naming tabs from one.

use iznik_protocol::delta::{Delta, ExitStatus as EndedAs, RemovalReason};
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::MarkKind;
use std::time::Duration;

use tokio::sync::broadcast;

use super::Registry;
use super::program::pane_text;
use crate::pty::spawn::ExitStatus;

/// How long after a pane's end is first seen its exit status is waited for
/// before the pane is reported as simply gone. The reaper records it within
/// milliseconds; a time rather than a count of looks, because how often
/// [`Registry::ingest`] is called says nothing about how long it has been.
pub(super) const EXIT_STATUS_DEADLINE: Duration = Duration::from_secs(1);

impl Registry {
    /// Turns everything the panes have reported since the last call into
    /// deltas: titles and working directories from their marks, sizes from
    /// their state, a foreground program when one is being sampled, and the
    /// removal cascade from an exit. It takes what is there and does not wait,
    /// so whoever owns the registry calls it when [`Registry::signal`] is raised.
    ///
    /// # Panics
    ///
    /// A pane whose child has ended is taken out of the model, and ending a
    /// pane escalates to `SIGKILL` on a task, so this must be called from
    /// within a Tokio runtime.
    pub fn ingest(&mut self) {
        let panes: Vec<PaneId> = self.watching.keys().copied().collect();
        for pane in panes {
            self.ingest_marks(pane);
            self.ingest_state(pane);
        }
        self.apply_programs();
        self.settled();
    }

    /// The deltas a pane's marks have become. A mark the model does not hold
    /// is a client's business, and the multiplexer forwards it.
    fn ingest_marks(&mut self, pane: PaneId) {
        let mut deltas = Vec::new();
        let mut shell_directory = false;
        if let Some(watching) = self.watching.get_mut(&pane) {
            loop {
                match watching.marks.try_recv() {
                    Ok(event) => match event.kind {
                        MarkKind::Title { text } => {
                            deltas.push(Delta::PaneTitle { pane, title: text });
                        }
                        MarkKind::WorkingDirectory { path } => {
                            shell_directory = true;
                            deltas.push(Delta::PaneWorkingDirectory { pane, path });
                        }
                        MarkKind::PromptStart
                        | MarkKind::CommandStart
                        | MarkKind::CommandExecuted
                        | MarkKind::CommandFinished { .. }
                        | MarkKind::AlternateScreen { .. } => {}
                    },
                    // A client that fell behind on marks still gets the model
                    // right; the ring is the durable record of the bytes.
                    Err(broadcast::error::TryRecvError::Lagged(_missed)) => {}
                    Err(_gone) => break,
                }
            }
        }
        if shell_directory && let Some(programs) = &self.programs {
            programs.note_shell_directory(pane);
        }
        for delta in deltas {
            // A program that sets the title it already has — a prompt that
            // names the terminal on every line — changes nothing, and every
            // client would otherwise be sent a delta for it every time.
            if !self.already_holds(&delta) {
                self.announce(delta);
            }
        }
    }

    /// Whether the model already says what a title or directory delta would
    /// make it say.
    fn already_holds(&self, delta: &Delta) -> bool {
        match delta {
            Delta::PaneTitle { pane, title } => {
                pane_text(&self.model, *pane).is_some_and(|(held, _directory)| held == *title)
            }
            Delta::PaneWorkingDirectory { pane, path } => pane_text(&self.model, *pane)
                .is_some_and(|(_title, held)| held.as_deref() == Some(path.as_str())),
            _other => false,
        }
    }

    /// The deltas a pane's size and end have become.
    fn ingest_state(&mut self, pane: PaneId) {
        let Some(watching) = self.watching.get_mut(&pane) else {
            return;
        };
        // A closed sender still holds the last state it published, and that is
        // the one that says the child has gone.
        if !watching.state.has_changed().unwrap_or(true) {
            return;
        }
        let state = *watching.state.borrow_and_update();
        let resized = (state.columns, state.rows) != watching.size;
        watching.size = (state.columns, state.rows);
        let ending = state.exited && !watching.ended;
        if resized {
            self.announce(Delta::PaneResized {
                pane,
                columns: state.columns,
                rows: state.rows,
            });
        }
        if !ending {
            return;
        }
        // The reaper records the status a moment after the stream closes. The
        // pane's going is not reported until it can be reported truthfully:
        // there is no code that stands in for a signal.
        let status = self
            .panes
            .get(&pane)
            .and_then(|held| held.exit_status_now());
        let reason = if let Some(status) = status {
            RemovalReason::Exited(ended_as(status))
        } else {
            let waited = self
                .watching
                .get_mut(&pane)
                .map_or(Duration::MAX, |recorded| {
                    recorded
                        .unexplained
                        .get_or_insert_with(tokio::time::Instant::now)
                        .elapsed()
                });
            if waited < EXIT_STATUS_DEADLINE {
                return;
            }
            // A reaper that cannot say how a child ended — one already reaped
            // by something else — would otherwise leave a dead pane in every
            // client's model for ever, with nothing able to reach the removal
            // path. It is gone and nobody can say how, and `Closed` is the
            // nearest true thing there is to say.
            tracing::warn!(
                pane = pane.0,
                "a pane ended and its status was never recorded"
            );
            RemovalReason::Closed
        };
        if let Some(recorded) = self.watching.get_mut(&pane) {
            recorded.ended = true;
        }
        self.remove_pane(pane, reason);
    }
}

/// How a child's end is told: a code it chose, or the signal's number.
fn ended_as(status: ExitStatus) -> EndedAs {
    match status {
        ExitStatus::Exited(code) => EndedAs::Exited(code),
        ExitStatus::Signalled(number) => EndedAs::Signalled(number),
    }
}
