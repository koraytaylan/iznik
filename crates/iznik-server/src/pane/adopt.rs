//! A pane built from a master this process inherited and the bytes the
//! previous one had already produced.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use iznik_protocol::identity::Sequence;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::vt::VtTask;
use super::{MARK_CHANNEL_CAPACITY, Pane, PaneError, PaneOptions, PaneState, reap_on_exit};
use crate::history::ring::PaneHistory;
use crate::pty::spawn::PtyProcess;
use crate::pty::streams::streams;
use crate::terminal::mirror::MirrorThread;

/// Keeps an inherited child alive when the pane around it cannot be built.
fn preserve<Held>(value: Held) {
    let _kept = Box::leak(Box::new(value));
}

/// A pane on `process` whose history and mirror start from `ring`.
///
/// `sequence` is the sequence just past the last byte of the pane's life, and
/// `ring` is what the history still holds of it. The mirror is fed `ring`
/// before it reads anything the child writes from now on.
///
/// # Errors
///
/// [`PaneError::Pty`] when the master cannot be turned into streams or the
/// reaper cannot start, and [`PaneError::Mirror`] when the mirror cannot be
/// created.
pub(super) async fn from_process(
    process: PtyProcess,
    history_bytes: usize,
    ring: &[u8],
    sequence: Sequence,
    columns: u16,
    rows: u16,
    thread: &MirrorThread,
) -> Result<Pane, PaneError> {
    // This process did not spawn the child. A failed build must not signal it:
    // the previous daemon is about to adopt the same master.
    let (output, input) = match streams(&process) {
        Ok(opened) => opened,
        Err(error) => {
            preserve(process);
            return Err(PaneError::Pty(error));
        }
    };
    let process = Arc::new(Mutex::new(process));
    let (exit_sender, exit) = watch::channel(None);
    if let Err(error) = reap_on_exit(Arc::clone(&process), exit_sender) {
        preserve(process);
        return Err(error);
    }

    let history = Arc::new(Mutex::new(PaneHistory::carrying(
        history_bytes,
        sequence,
        ring,
    )));
    let (oldest, newest) = {
        let held = history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (held.oldest(), held.newest())
    };
    let (marks, _receiver) = broadcast::channel(MARK_CHANNEL_CAPACITY);
    let (requests, requests_receiver) = mpsc::unbounded_channel();
    let initial = PaneState {
        columns,
        rows,
        newest,
        oldest,
        exited: false,
        prompts: 0,
    };
    let (state_sender, state) = watch::channel(initial);
    let (ready_sender, ready_receiver) = oneshot::channel();
    let pane_options = PaneOptions::default();
    let task = VtTask {
        columns,
        rows,
        output,
        responses: input.clone(),
        history: Arc::clone(&history),
        marks: marks.clone(),
        state: state_sender,
        requests: requests_receiver,
        exit: exit.clone(),
        drain: pane_options.exit_drain,
        replay: ring.to_vec(),
        ready: ready_sender,
    };
    thread.spawn(move || task.run());
    match ready_receiver.await {
        Ok(Ok(())) => {}
        Ok(Err(source)) => {
            preserve(process);
            return Err(PaneError::Mirror(source));
        }
        Err(_recv) => {
            preserve(process);
            return Err(PaneError::Gone);
        }
    }
    Ok(Pane {
        input,
        history,
        state,
        marks,
        requests,
        process,
        options: pane_options,
        exit,
        closing: AtomicBool::new(false),
    })
}
