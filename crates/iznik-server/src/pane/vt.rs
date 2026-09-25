//! The VT task: the one place a pane's mirror is touched. It runs on the
//! mirror thread, feeds the mirror every chunk the child writes, keeps the
//! history ring and the marks, writes the mirror's query answers back to the
//! child while no client is subscribed, and answers the pane's requests —
//! a screen, a resize, a subscription beginning or ending.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iznik_protocol::identity::Sequence;
use iznik_protocol::message::MarkKind;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::{Begun, PaneState, Request};
use crate::history::ring::{CarriedRing, PaneHistory};
use crate::pty::spawn::ExitStatus;
use crate::pty::streams::{InputHandle, OutputStream};
use crate::terminal::marks::{MarkEvent, MarkObserver};
use crate::terminal::mirror::{Mirror, MirrorError};
use crate::terminal::screen::{ScreenError, ScreenState};

/// The alternate-screen enter to remember for reconstruction when the recognized
/// switch began in an earlier read, so its own bytes are not wholly in the chunk
/// the pane splits at it. A client applying it reaches the alternate screen.
const CANONICAL_ALTERNATE_ENTER: &[u8] = b"\x1b[?1049h";

/// Everything the VT task owns off the mirror thread; its mirror is built on the
/// thread when the task first runs, because it is `!Send`.
pub(super) struct VtTask {
    /// The pane's initial width.
    pub(super) columns: u16,
    /// The pane's initial height.
    pub(super) rows: u16,
    /// The child's output.
    pub(super) output: OutputStream,
    /// The child's input, for writing the mirror's query answers back.
    pub(super) responses: InputHandle,
    /// The history ring to append to.
    pub(super) history: Arc<Mutex<PaneHistory>>,
    /// The mark events to emit.
    pub(super) marks: broadcast::Sender<MarkEvent>,
    /// The state to publish on every change.
    pub(super) state: watch::Sender<PaneState>,
    /// Requests from the pane.
    pub(super) requests: mpsc::UnboundedReceiver<Request>,
    /// The shell's exit status, set by the reaper whether or not the output
    /// has closed.
    pub(super) exit: watch::Receiver<Option<ExitStatus>>,
    /// How long to keep reading once the shell has been reaped.
    pub(super) drain: Duration,
    /// Bytes the pane already produced, fed to the mirror before anything new
    /// arrives. Empty for a pane this process spawned.
    pub(super) replay: Vec<u8>,
    /// Signals whether the mirror was created, so the spawn fails if it was not.
    pub(super) ready: oneshot::Sender<Result<(), MirrorError>>,
}

impl VtTask {
    /// Runs the task: build the mirror, then feed it every chunk and answer every
    /// request until the output closes — or, once the shell has been reaped,
    /// until the drain after it runs out, because a background job holding the
    /// terminal would keep the output open for as long as it lives.
    pub(super) async fn run(self) {
        let VtTask {
            columns,
            rows,
            mut output,
            responses,
            history,
            marks,
            state,
            mut requests,
            mut exit,
            drain,
            replay,
            ready,
        } = self;
        // Held for the whole run: however the task ends — the output
        // closing, the drain running out, or a panic unwinding it off the
        // mirror thread — the pane is published as ended, so the registry
        // takes it away rather than holding a pane nothing feeds.
        let state = EndsExited(state);
        let state = &state.0;
        let mut mirror = match Mirror::new(columns, rows) {
            Ok(mirror) => {
                let _sent = ready.send(Ok(()));
                mirror
            }
            Err(source) => {
                let _sent = ready.send(Err(source));
                return;
            }
        };
        // The carried ring, through the same emulator a live chunk uses. The
        // history already holds it, so it is not appended again, and any
        // answer the emulator would write is discarded: the child was answered
        // when the bytes were first produced.
        if !replay.is_empty() {
            mirror.feed(&replay);
            let _discarded = mirror.take_pending_responses();
        }
        let mut live = Live {
            output: &mut output,
            responses: &responses,
            history: &history,
            marks: &marks,
            state,
            requests: &mut requests,
            exit: &mut exit,
            drain,
            mirror,
            observer: MarkObserver::new(),
            screen_state: ScreenState::new(),
            prompts: 0,
            subscribers: 0,
            answered_through: Sequence(0),
            requests_open: true,
            exit_open: true,
            ending: None,
        };
        live.serve().await;
    }
}

/// The mirror and the counters a running VT task mutates.
struct Live<'task> {
    /// The child's output.
    output: &'task mut OutputStream,
    /// Where query answers are written.
    responses: &'task InputHandle,
    /// The history ring.
    history: &'task Arc<Mutex<PaneHistory>>,
    /// Mark events.
    marks: &'task broadcast::Sender<MarkEvent>,
    /// The published pane state.
    state: &'task watch::Sender<PaneState>,
    /// Requests from the pane.
    requests: &'task mut mpsc::UnboundedReceiver<Request>,
    /// The shell's exit, once the reaper has one.
    exit: &'task mut watch::Receiver<Option<ExitStatus>>,
    /// How long to keep reading after the shell is reaped.
    drain: Duration,
    /// The emulator.
    mirror: Mirror,
    /// Mark observation.
    observer: MarkObserver,
    /// Primary-screen memory.
    screen_state: ScreenState,
    /// Prompts observed so far.
    prompts: u64,
    /// Clients subscribed now.
    subscribers: usize,
    /// How far the mirror has answered queries.
    answered_through: Sequence,
    /// Whether the request channel is still open.
    requests_open: bool,
    /// Whether the exit watch is still open.
    exit_open: bool,
    /// When the drain after the shell's exit ends.
    ending: Option<tokio::time::Instant>,
}

impl Live<'_> {
    /// Feeds output and answers requests until the pane ends.
    async fn serve(&mut self) {
        loop {
            if self.turn().await.is_none() {
                break;
            }
        }
        publish(self.state, self.history, &self.mirror, true, self.prompts);
    }

    /// One event. `None` ends the pane.
    async fn turn(&mut self) -> Option<()> {
        let deadline = self.ending.unwrap_or_else(tokio::time::Instant::now);
        // Requests before output: a resize queued before the child was told
        // of it reaches the mirror before anything the child drew for it.
        tokio::select! {
            biased;
            request = self.requests.recv(), if self.requests_open => {
                self.on_request(request).await;
                Some(())
            }
            changed = self.exit.changed(), if self.exit_open && self.ending.is_none() => {
                self.on_exit(&changed)
            }
            () = tokio::time::sleep_until(deadline), if self.ending.is_some() => None,
            chunk = self.output.next() => self.on_chunk(chunk),
        }
    }

    /// Applies one request.
    async fn on_request(&mut self, request: Option<Request>) {
        match request {
            Some(Request::Screen(reply)) => {
                let sequence = newest_of(self.history);
                let _sent = reply.send(self.screen_state.serialize(&self.mirror, sequence));
            }
            Some(Request::Resize { columns, rows }) => {
                self.mirror.resize(columns, rows);
                publish(self.state, self.history, &self.mirror, false, self.prompts);
            }
            Some(Request::Subscribe { screen, reply }) => {
                self.subscribers = self.subscribers.saturating_add(1);
                self.mirror.set_subscriber_count(self.subscribers);
                let begun = begin(
                    &self.mirror,
                    &self.screen_state,
                    self.history,
                    screen,
                    self.answered_through,
                );
                let _sent = reply.send(begun);
            }
            Some(Request::Unsubscribe) => {
                self.subscribers = self.subscribers.saturating_sub(1);
                self.mirror.set_subscriber_count(self.subscribers);
            }
            Some(Request::Quiesce(reply)) => {
                let carried = self.quiesce().await;
                let _sent = reply.send(carried);
            }
            Some(Request::Resume) => self.output.resume(),
            None => self.requests_open = false,
        }
    }

    /// Notes that the shell has been reaped, and starts the drain.
    ///
    /// `None` when the drain cannot be scheduled, which ends the pane.
    fn on_exit(&mut self, changed: &Result<(), watch::error::RecvError>) -> Option<()> {
        if changed.is_err() {
            self.exit_open = false;
            return Some(());
        }
        if self.exit.borrow_and_update().is_some() {
            self.ending = tokio::time::Instant::now().checked_add(self.drain);
            self.ending?;
        }
        Some(())
    }

    /// Feeds one chunk. `None` when the output has closed, which ends the pane.
    fn on_chunk(&mut self, chunk: Option<Vec<u8>>) -> Option<()> {
        let bytes = chunk?;
        let prompted = feed_chunk(
            &bytes,
            &mut self.mirror,
            &mut self.observer,
            &mut self.screen_state,
            self.history,
            self.marks,
            self.responses,
        );
        self.prompts = self.prompts.saturating_add(prompted);
        if self.subscribers == 0 {
            self.answered_through = newest_of(self.history);
        }
        publish(self.state, self.history, &self.mirror, false, self.prompts);
        Some(())
    }

    /// Stops the reader, feeds every chunk it had already queued, and copies the ring.
    async fn quiesce(&mut self) -> CarriedRing {
        self.output.pause().await;
        while let Some(bytes) = self.output.try_next() {
            let prompted = feed_chunk(
                &bytes,
                &mut self.mirror,
                &mut self.observer,
                &mut self.screen_state,
                self.history,
                self.marks,
                self.responses,
            );
            self.prompts = self.prompts.saturating_add(prompted);
            publish(self.state, self.history, &self.mirror, false, self.prompts);
        }
        self.history
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tail()
    }
}

/// A pane's state sender that says the pane has ended when it is dropped,
/// including when a panic unwinds the task that holds it.
struct EndsExited(watch::Sender<PaneState>);

impl Drop for EndsExited {
    fn drop(&mut self) {
        self.0.send_modify(|state| state.exited = true);
    }
}

/// What a subscription is told as it begins: how far the mirror answered,
/// and the screen at this instant when it was asked for.
///
/// # Errors
///
/// The serializer's, when the screen was asked for and cannot be made.
fn begin(
    mirror: &Mirror,
    screen_state: &ScreenState,
    history: &Arc<Mutex<PaneHistory>>,
    screen: bool,
    answered_through: Sequence,
) -> Result<Begun, ScreenError> {
    let screen = if screen {
        Some(screen_state.serialize(mirror, newest_of(history))?)
    } else {
        None
    };
    Ok(Begun {
        answered_through,
        screen,
    })
}

/// The newest sequence the history holds.
fn newest_of(history: &Arc<Mutex<PaneHistory>>) -> Sequence {
    history
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .newest()
}

/// Appends `bytes` to history, observes their marks, feeds them to the mirror —
/// remembering the primary screen at each alternate-screen entry — writes the
/// mirror's answers back to the child when no client is subscribed, and emits the
/// marks.
///
/// Answers how many of those marks said the shell was about to print a prompt,
/// which the caller adds to what the pane publishes: a mark is heard once and
/// only by whoever was already listening, and the count is what is left to
/// read afterwards.
fn feed_chunk(
    bytes: &[u8],
    mirror: &mut Mirror,
    observer: &mut MarkObserver,
    screen_state: &mut ScreenState,
    history: &Arc<Mutex<PaneHistory>>,
    marks: &broadcast::Sender<MarkEvent>,
    responses: &InputHandle,
) -> u64 {
    let base = history
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .append(bytes);
    let events = observer.observe(base, bytes);

    // Feed the chunk, splitting it at each alternate-screen switch in order. The
    // alternate-screen state is tracked from the events (seeded from the mirror),
    // not re-read mid-chunk, so several switches in one chunk — enter, leave,
    // enter — each act at the right instant and in the right order.
    let mut fed = 0_usize;
    let mut in_alternate = mirror.in_alternate_screen();
    for event in &events {
        let MarkKind::AlternateScreen { entered } = event.kind else {
            continue;
        };
        // The switch's own bytes may begin in a previous read; then `start`
        // clamps to what is already fed and the whole switch is not in this chunk.
        let whole_in_chunk = event.sequence.0 >= base.0;
        let start = usize::try_from(event.sequence.0.saturating_sub(base.0))
            .unwrap_or(bytes.len())
            .clamp(fed, bytes.len());
        let end = usize::try_from(
            event
                .sequence
                .0
                .saturating_add(u64::try_from(event.length).unwrap_or(0))
                .saturating_sub(base.0),
        )
        .unwrap_or(bytes.len())
        .clamp(start, bytes.len());

        // Everything up to the switch, so the primary is still active for a snapshot.
        if let Some(segment) = bytes.get(fed..start) {
            mirror.feed(segment);
            drain_responses(mirror, responses);
        }
        if entered && !in_alternate {
            // Remember the primary before the switch is applied, with the switch's
            // own bytes when they are wholly here, else a canonical enter.
            let switch = match bytes.get(start..end) {
                Some(recognized) if whole_in_chunk => recognized.to_vec(),
                _ => CANONICAL_ALTERNATE_ENTER.to_vec(),
            };
            let _remembered = screen_state.entering_alternate(mirror, &switch);
        }
        // The switch itself, applying the transition.
        if let Some(segment) = bytes.get(start..end) {
            mirror.feed(segment);
            drain_responses(mirror, responses);
        }
        if !entered && in_alternate {
            screen_state.leaving_alternate();
        }
        in_alternate = entered;
        fed = end;
    }
    if let Some(rest) = bytes.get(fed..) {
        mirror.feed(rest);
        drain_responses(mirror, responses);
    }
    let mut prompted = 0_u64;
    for event in events {
        if matches!(event.kind, MarkKind::PromptStart) {
            prompted = prompted.saturating_add(1);
        }
        let _sent = marks.send(event);
    }
    prompted
}

/// Writes the mirror's pending query answers back to the child's input. The
/// mirror accumulates them only when no client is subscribed, so a subscribed
/// pane drains nothing here and the client's own emulator answers.
fn drain_responses(mirror: &mut Mirror, responses: &InputHandle) {
    let pending = mirror.take_pending_responses();
    if !pending.is_empty() {
        let _written = responses.write(pending);
    }
}

/// Publishes the pane's current state.
fn publish(
    state: &watch::Sender<PaneState>,
    history: &Arc<Mutex<PaneHistory>>,
    mirror: &Mirror,
    exited: bool,
    prompts: u64,
) {
    let (newest, oldest) = {
        let history = history.lock().unwrap_or_else(PoisonError::into_inner);
        (history.newest(), history.oldest())
    };
    let _sent = state.send(PaneState {
        columns: mirror.columns(),
        rows: mirror.rows(),
        newest,
        oldest,
        exited,
        prompts,
    });
}
