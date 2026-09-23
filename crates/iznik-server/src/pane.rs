//! The pane: pseudoterminal, mirror, history ring and mark observer held
//! together by one VT task, behind one interface.
//!
//! A pane is four things held together by one event stream — a pseudoterminal
//! and its child, a `libghostty-vt` mirror, a bounded history ring, and a
//! shell-integration observer. The mirror is `!Send`, so the task that feeds it
//! lives on the [`MirrorThread`]; everything else — input, history reads, state,
//! marks, resize and exit — is reachable from anywhere.
//!
//! The ring is the queue: a subscriber learns that new bytes exist through
//! [`Pane::state`] and reads them at its own cursor with [`Pane::read_history`];
//! nothing is pushed to a subscriber that cannot take it. The mirror answers a
//! program's terminal queries only while no client is subscribed, because an
//! attached client's own emulator answers; the task writes those answers back to
//! the child's input after each fed chunk.
//!
//! Which side answers is decided per byte, at the moment the mirror is fed it,
//! and the task remembers how far the mirror has answered: every byte before
//! that sequence had its queries answered here. A subscription is told that
//! sequence at the very instant it begins — in the same turn of the task, with
//! the screen when one is asked for — so a client that is then sent older
//! bytes knows its own answers to them would be second ones.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iznik_protocol::identity::Sequence;
use iznik_protocol::message::MarkKind;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::history::ring::{HistoryError, PaneHistory};
use crate::pty::spawn::{ExitStatus, PtyError, PtyProcess, SpawnOptions, spawn};
use crate::pty::streams::{InputError, InputHandle, OutputStream, streams};
use crate::terminal::marks::{MarkEvent, MarkObserver};
use crate::terminal::mirror::{Mirror, MirrorError, MirrorThread};
use crate::terminal::screen::{ScreenError, ScreenState, SerializedScreen};

/// How many mark events a subscriber can fall behind before the oldest are lost;
/// a client reads marks promptly, and the ring is the durable record regardless.
const MARK_CHANNEL_CAPACITY: usize = 1024;

/// How long [`Pane::close`] waits for a hung-up child to exit before it escalates
/// to `SIGKILL`, so a shell that ignores `SIGHUP` is still ended.
const CLOSE_ESCALATION: Duration = Duration::from_secs(2);

/// How long the VT task keeps reading after the shell has been reaped without
/// its output closing. The shell's last words are already in the terminal and
/// arrive within milliseconds; a background job still holding the terminal
/// open is what would otherwise keep the pane alive for as long as it runs.
const EXIT_DRAIN: Duration = Duration::from_millis(250);

/// The widest a pane may be. A client asks for a size and the server makes a
/// terminal of it — a mirror whose grid and scrollback are sized by it — so
/// the size is bounded here rather than trusted: a thousand columns is wider
/// than any display shows at a readable size.
pub const MAXIMUM_COLUMNS: u16 = 1000;

/// The tallest a pane may be, for the same reason.
pub const MAXIMUM_ROWS: u16 = 500;

/// A size a pane can be made at: at least one cell each way, and at most
/// [`MAXIMUM_COLUMNS`] by [`MAXIMUM_ROWS`].
#[must_use]
pub fn bounded_size(columns: u16, rows: u16) -> (u16, u16) {
    (
        columns.clamp(1, MAXIMUM_COLUMNS),
        rows.clamp(1, MAXIMUM_ROWS),
    )
}

/// The alternate-screen enter to remember for reconstruction when the recognized
/// switch began in an earlier read, so its own bytes are not wholly in the chunk
/// the pane splits at it. A client applying it reaches the alternate screen.
const CANONICAL_ALTERNATE_ENTER: &[u8] = b"\x1b[?1049h";

/// What a pane looks like right now, published on every change: its size, the
/// bounds of its history, and whether its child has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneState {
    /// The pane's width in columns.
    pub columns: u16,
    /// The pane's height in rows.
    pub rows: u16,
    /// The sequence just past the newest byte the pane has produced.
    pub newest: Sequence,
    /// The sequence of the oldest byte still held in history.
    pub oldest: Sequence,
    /// Whether the child has ended and its output stream closed.
    pub exited: bool,
    /// How many prompts the shell has said it was about to print.
    ///
    /// Counted where the mark is sent, so a pane that has prompted once is one
    /// whose emulator has been fed the bytes that said so. Marks are an event
    /// and this is not: a client that subscribed after the shell came up would
    /// wait for a first prompt that had already happened, where reading this
    /// says what has happened whenever it is asked.
    pub prompts: u64,
}

/// Why a pane operation failed.
#[derive(Debug)]
pub enum PaneError {
    /// The pseudoterminal could not be opened, spawned or operated.
    Pty(PtyError),
    /// The mirror could not be created on its thread.
    Mirror(MirrorError),
    /// Input could not be accepted.
    Input(InputError),
    /// The screen could not be serialized.
    Screen(ScreenError),
    /// History from before the oldest byte held was requested.
    History(HistoryError),
    /// The mirror thread ended before it answered.
    Gone,
}

impl core::fmt::Display for PaneError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PaneError::Pty(source) => write!(formatter, "the pseudoterminal: {source}"),
            PaneError::Mirror(source) => write!(formatter, "the mirror: {source}"),
            PaneError::Input(source) => write!(formatter, "the input: {source}"),
            PaneError::Screen(source) => write!(formatter, "the screen: {source}"),
            PaneError::History(source) => write!(formatter, "the history: {source}"),
            PaneError::Gone => write!(formatter, "the mirror thread has ended"),
        }
    }
}

impl std::error::Error for PaneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PaneError::Pty(source) => Some(source),
            PaneError::Mirror(source) => Some(source),
            PaneError::Input(source) => Some(source),
            PaneError::Screen(source) => Some(source),
            PaneError::History(source) => Some(source),
            PaneError::Gone => None,
        }
    }
}

impl From<PtyError> for PaneError {
    fn from(source: PtyError) -> PaneError {
        PaneError::Pty(source)
    }
}

impl From<InputError> for PaneError {
    fn from(source: InputError) -> PaneError {
        PaneError::Input(source)
    }
}

impl From<HistoryError> for PaneError {
    fn from(source: HistoryError) -> PaneError {
        PaneError::History(source)
    }
}

/// A request from the pane to its VT task, which alone touches the mirror.
enum Request {
    /// Serialize the screen, exact at the newest sequence, back over the channel.
    Screen(oneshot::Sender<Result<SerializedScreen, ScreenError>>),
    /// Resize the mirror, ahead of the pseudoterminal's own resize.
    Resize {
        /// The new width in columns.
        columns: u16,
        /// The new height in rows.
        rows: u16,
    },
    /// A client subscribed, so the mirror stops answering the child's queries;
    /// answered with where the mirror's own answering stopped and, when asked
    /// for, the screen at that same instant.
    Subscribe {
        /// Whether to serialize the screen in the same turn.
        screen: bool,
        /// Where the answer goes.
        reply: oneshot::Sender<Result<Begun, ScreenError>>,
    },
    /// A client unsubscribed.
    Unsubscribe,
}

/// A client's subscription to a pane. While it lives, the pane counts one more
/// subscriber and stops answering the child's terminal queries itself; dropping
/// it releases the count.
#[derive(Debug)]
pub struct Subscription {
    /// The channel to the VT task, to release the subscription on drop.
    requests: mpsc::UnboundedSender<Request>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _sent = self.requests.send(Request::Unsubscribe);
    }
}

/// What the VT task says when a subscription begins.
#[derive(Debug)]
struct Begun {
    /// Every byte before this had its queries answered by the mirror.
    answered_through: Sequence,
    /// The screen at the same instant, when it was asked for.
    screen: Option<SerializedScreen>,
}

/// A subscription that has begun: the guard that keeps it, how far the mirror
/// had answered the child's queries when it began, and the screen at that
/// instant when one was asked for.
#[derive(Debug)]
pub struct Attachment {
    /// Held for as long as the client watches the pane.
    pub subscription: Subscription,
    /// The mirror answered every terminal query in the bytes before this
    /// sequence, and answers none after it while the subscription lives. A
    /// client sent bytes from before it must not answer their queries again.
    pub answered_through: Sequence,
    /// The screen, exact at the moment the subscription began — so no chunk
    /// the mirror was fed silently falls between the two.
    pub screen: Option<SerializedScreen>,
}

/// A pane: a pseudoterminal with a login shell, mirrored and observed, its
/// history kept, behind one interface. Dropping it kills the child's process
/// group and the terminal foreground group — unless [`Pane::close`] has
/// already begun, in which case the close's own escalation ends them after
/// its grace period; jobs detached from both groups are beyond terminal group
/// signaling.
#[derive(Debug)]
pub struct Pane {
    /// The child's input; each write is put through whole.
    input: InputHandle,
    /// The history ring, appended by the VT task and read here.
    history: Arc<Mutex<PaneHistory>>,
    /// The latest published state.
    state: watch::Receiver<PaneState>,
    /// The mark events, for a client to subscribe to.
    marks: broadcast::Sender<MarkEvent>,
    /// The channel to the VT task.
    requests: mpsc::UnboundedSender<Request>,
    /// The child process, shared with the reaper that waits for its exit.
    process: Arc<Mutex<PtyProcess>>,
    /// Runtime timing; tests shorten the close grace period.
    options: PaneOptions,
    /// The child's exit status, once it has ended.
    exit: watch::Receiver<Option<ExitStatus>>,
    /// Whether [`Pane::close`] has hung the terminal up and handed the rest to
    /// its escalation, so dropping the pane must not cut the grace short.
    closing: AtomicBool,
}

impl Pane {
    /// Spawns a pane: a pseudoterminal running `options`, its history ring
    /// `history_bytes` large, its mirror and VT task on `thread`.
    ///
    /// # Errors
    ///
    /// [`PaneError::Pty`] when the pseudoterminal cannot be opened or spawned,
    /// and [`PaneError::Mirror`] when the mirror cannot be created on its thread.
    pub async fn spawn(
        options: &SpawnOptions,
        history_bytes: usize,
        thread: &MirrorThread,
    ) -> Result<Pane, PaneError> {
        Self::spawn_with_options(options, history_bytes, thread, PaneOptions::default()).await
    }

    /// Spawn with caller-selected timing while retaining the production defaults
    /// in [`Self::spawn`].
    ///
    /// # Errors
    /// Returns the same PTY and mirror initialization failures as [`Self::spawn`].
    pub async fn spawn_with_options(
        options: &SpawnOptions,
        history_bytes: usize,
        thread: &MirrorThread,
        pane_options: PaneOptions,
    ) -> Result<Pane, PaneError> {
        let (columns, rows) = bounded_size(options.columns, options.rows);
        // Off the runtime's workers: a fork, an exec and the directory checks
        // on the way to them are blocking system calls, however quick.
        let mut owned = options.clone();
        owned.columns = columns;
        owned.rows = rows;
        let (process, output, input) = tokio::task::spawn_blocking(move || {
            let process = spawn(&owned)?;
            let (output, input) = streams(&process)?;
            Ok::<_, PtyError>((process, output, input))
        })
        .await
        .map_err(|source| {
            PaneError::Pty(PtyError::Open {
                source: source.into(),
            })
        })??;
        let process = Arc::new(Mutex::new(process));
        let (exit_tx, exit) = watch::channel(None);
        reap_on_exit(Arc::clone(&process), exit_tx)?;

        let history = Arc::new(Mutex::new(PaneHistory::new(history_bytes)));
        let (marks, _marks_rx) = broadcast::channel(MARK_CHANNEL_CAPACITY);
        let (requests, requests_rx) = mpsc::unbounded_channel();
        let initial = PaneState {
            columns,
            rows,
            newest: Sequence(0),
            oldest: Sequence(0),
            exited: false,
            prompts: 0,
        };
        let (state_tx, state) = watch::channel(initial);

        // The VT task builds its mirror on the thread and signals readiness, so a
        // mirror that cannot be created fails the spawn rather than dying silently.
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = VtTask {
            columns,
            rows,
            output,
            responses: input.clone(),
            history: Arc::clone(&history),
            marks: marks.clone(),
            state: state_tx,
            requests: requests_rx,
            exit: exit.clone(),
            drain: pane_options.exit_drain,
            ready: ready_tx,
        };
        thread.spawn(move || task.run());
        match ready_rx.await {
            Ok(Ok(())) => {}
            Ok(Err(source)) => return Err(PaneError::Mirror(source)),
            Err(_recv) => return Err(PaneError::Gone),
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

    /// Enqueues input to the child, put through whole.
    ///
    /// # Errors
    ///
    /// [`PaneError::Input`] when the input is backed up or the child has closed.
    pub fn input(&self, bytes: Vec<u8>) -> Result<(), PaneError> {
        self.input.write(bytes).map_err(PaneError::Input)
    }

    /// The pane's latest state.
    #[must_use]
    pub fn state(&self) -> PaneState {
        *self.state.borrow()
    }

    /// The child's process id.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .process_id()
    }

    /// The foreground process, or the shell when the shell is in the foreground.
    #[must_use]
    pub fn foreground_process_id(&self) -> u32 {
        self.process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .foreground_process_id()
    }

    /// The bytes from `from` to the newest, as one vector. The copy is made while
    /// the history lock is held, so it is consistent; a subscriber reads
    /// incrementally from its own cursor, keeping the copy — and the lock — short,
    /// and only a full-ring read on a cold reconnect briefly stalls the mirror
    /// thread's next append.
    ///
    /// # Errors
    ///
    /// [`PaneError::History`] when `from` is older than the oldest byte held.
    pub fn read_history(&self, from: Sequence) -> Result<Vec<u8>, PaneError> {
        let history = self.history.lock().unwrap_or_else(PoisonError::into_inner);
        let mut bytes = Vec::new();
        history.copy_range(from, usize::MAX, &mut bytes)?;
        Ok(bytes)
    }

    /// At most `limit` bytes from `from`, appended to a buffer the caller
    /// owns. This is what the multiplexer reads with: a whole-ring read would
    /// be megabytes for one frame, and the pump is specified to buffer no pane
    /// bytes of its own beyond the one frame it is sending.
    ///
    /// # Errors
    ///
    /// [`PaneError::History`] when `from` is older than the oldest byte held.
    pub fn copy_history(
        &self,
        from: Sequence,
        limit: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), PaneError> {
        let history = self.history.lock().unwrap_or_else(PoisonError::into_inner);
        history.copy_range(from, limit, out)?;
        Ok(())
    }

    /// Every change to what the pane looks like, so a watcher is woken rather
    /// than polled. [`Pane::state`] is the same thing for a caller that only
    /// wants to know now.
    #[must_use]
    pub fn state_updates(&self) -> watch::Receiver<PaneState> {
        self.state.clone()
    }

    /// How much history the pane keeps, set to what the shared budget allows.
    ///
    /// This is how the budget takes memory back: it decides how much each pane
    /// may hold and the registry tells the pane, because the ring the pane
    /// appends to is the pane's own and nothing else can reach it.
    pub fn set_history_capacity(&self, capacity: usize) {
        let mut history = self.history.lock().unwrap_or_else(PoisonError::into_inner);
        history.set_capacity(capacity);
    }

    /// The child's exit status if it has already ended, without waiting for
    /// one that has not. [`PaneState::exited`] is what says there is one.
    ///
    /// [`Pane::exit_status`] is the same answer for a caller that can wait;
    /// the registry cannot, because it turns an exit into deltas while it
    /// holds the model.
    #[must_use]
    pub fn exit_status_now(&self) -> Option<ExitStatus> {
        *self.exit.borrow()
    }

    /// The mirror's screen serialized as VT bytes, exact at its sequence.
    ///
    /// # Errors
    ///
    /// [`PaneError::Screen`] when the emulator's formatter fails, and
    /// [`PaneError::Gone`] when the mirror thread has ended.
    pub async fn screen(&self) -> Result<SerializedScreen, PaneError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.requests
            .send(Request::Screen(reply_tx))
            .map_err(|_send| PaneError::Gone)?;
        match reply_rx.await {
            Ok(Ok(screen)) => Ok(screen),
            Ok(Err(source)) => Err(PaneError::Screen(source)),
            Err(_recv) => Err(PaneError::Gone),
        }
    }

    /// A receiver of this pane's mark events from now on.
    #[must_use]
    pub fn marks(&self) -> broadcast::Receiver<MarkEvent> {
        self.marks.subscribe()
    }

    /// Registers a client subscription, so the pane stops answering the child's
    /// terminal queries itself until the subscription is dropped, and says
    /// how far it had answered them — with the screen at that same instant
    /// when `screen` asks for it.
    ///
    /// # Errors
    ///
    /// [`PaneError::Screen`] when the screen was asked for and cannot be
    /// serialized, and [`PaneError::Gone`] when the mirror thread has ended.
    /// Either way nothing stays subscribed.
    pub async fn subscribe(&self, screen: bool) -> Result<Attachment, PaneError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.requests
            .send(Request::Subscribe {
                screen,
                reply: reply_tx,
            })
            .map_err(|_send| PaneError::Gone)?;
        // Made before the answer is awaited: a caller that gives up, or an
        // answer that is a refusal, drops it and takes the count back down.
        let subscription = Subscription {
            requests: self.requests.clone(),
        };
        match reply_rx.await {
            Ok(Ok(begun)) => Ok(Attachment {
                subscription,
                answered_through: begun.answered_through,
                screen: begun.screen,
            }),
            Ok(Err(source)) => Err(PaneError::Screen(source)),
            Err(_recv) => Err(PaneError::Gone),
        }
    }

    /// Resizes the mirror and then the pseudoterminal — which sends the child
    /// `SIGWINCH` — so the next screen is at the new size, within
    /// [`bounded_size`].
    ///
    /// The mirror's resize is queued first. The VT task takes a queued request
    /// before the next chunk of output, and whatever the child draws for its
    /// new size comes after the `SIGWINCH` that the pseudoterminal's resize
    /// sends, so the mirror is already at that size when those bytes reach
    /// it — the other way round, a redraw could land on the old grid.
    ///
    /// # Errors
    ///
    /// [`PaneError::Pty`] when the pseudoterminal cannot be resized, and
    /// [`PaneError::Gone`] when the mirror thread has ended.
    pub fn resize(&self, columns: u16, rows: u16) -> Result<(), PaneError> {
        let (columns, rows) = bounded_size(columns, rows);
        self.requests
            .send(Request::Resize { columns, rows })
            .map_err(|_send| PaneError::Gone)?;
        let process = self.process.lock().unwrap_or_else(PoisonError::into_inner);
        process.resize(columns, rows)?;
        Ok(())
    }

    /// Hang up the foreground job, then force cleanup after the configured grace
    /// period. Keep the shell session alive during that grace period so an
    /// ignoring foreground job remains discoverable through the terminal.
    /// [`Pane::exit_status`] resolves once the child has gone. Call within Tokio.
    ///
    /// The escalation holds the child itself, so the pane may be dropped at
    /// once — as the registry does when it forgets a pane — without the drop
    /// killing what the grace period was for.
    ///
    /// # Errors
    /// Returns `Pty` when the initial hangup cannot be sent.
    pub fn close(&self) -> Result<(), PaneError> {
        self.process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .hangup_terminal()
            .map_err(PaneError::Pty)?;
        let process = Arc::clone(&self.process);
        let delay = self.options.close_escalation;
        let mut exit = self.exit.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if exit.borrow_and_update().is_none() {
                process
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .kill_terminal_groups();
            }
        });
        self.closing.store(true, Ordering::Release);
        Ok(())
    }

    /// How long [`Pane::close`] waits after hanging up before it kills.
    #[must_use]
    pub fn close_grace(&self) -> Duration {
        self.options.close_escalation
    }

    /// Kills the child's process group and the terminal's foreground group
    /// now, without a grace period: what a close escalates to, for a caller
    /// that cannot leave the escalation to a task — a daemon about to exit.
    pub fn kill(&self) {
        self.process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kill_terminal_groups();
    }

    /// Resolves to the child's exit status once it has ended, or `None` if the
    /// reaper was lost before it could report one.
    pub async fn exit_status(&self) -> Option<ExitStatus> {
        let mut exit = self.exit.clone();
        loop {
            if let Some(status) = *exit.borrow_and_update() {
                return Some(status);
            }
            if exit.changed().await.is_err() {
                return None;
            }
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // A close in progress owns the ending: its escalation kills after the
        // grace period, and killing here would take that grace away.
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        // The child wait owns no process mutex, so cleanup cannot wait behind it.
        self.kill();
    }
}

/// Pane lifecycle timing, configurable without changing the terminal's spawn geometry.
#[derive(Clone, Copy, Debug)]
pub struct PaneOptions {
    /// Grace after foreground hangup before forcing foreground and shell cleanup.
    pub close_escalation: Duration,
    /// How long output is still read once the shell has been reaped while
    /// something else holds its terminal open.
    pub exit_drain: Duration,
}

impl Default for PaneOptions {
    fn default() -> Self {
        Self {
            close_escalation: CLOSE_ESCALATION,
            exit_drain: EXIT_DRAIN,
        }
    }
}

/// Everything the VT task owns off the mirror thread; its mirror is built on the
/// thread when the task first runs, because it is `!Send`.
struct VtTask {
    /// The pane's initial width.
    columns: u16,
    /// The pane's initial height.
    rows: u16,
    /// The child's output.
    output: OutputStream,
    /// The child's input, for writing the mirror's query answers back.
    responses: InputHandle,
    /// The history ring to append to.
    history: Arc<Mutex<PaneHistory>>,
    /// The mark events to emit.
    marks: broadcast::Sender<MarkEvent>,
    /// The state to publish on every change.
    state: watch::Sender<PaneState>,
    /// Requests from the pane.
    requests: mpsc::UnboundedReceiver<Request>,
    /// The shell's exit status, set by the reaper whether or not the output
    /// has closed.
    exit: watch::Receiver<Option<ExitStatus>>,
    /// How long to keep reading once the shell has been reaped.
    drain: Duration,
    /// Signals whether the mirror was created, so the spawn fails if it was not.
    ready: oneshot::Sender<Result<(), MirrorError>>,
}

impl VtTask {
    /// Runs the task: build the mirror, then feed it every chunk and answer every
    /// request until the output closes — or, once the shell has been reaped,
    /// until the drain after it runs out, because a background job holding the
    /// terminal would keep the output open for as long as it lives.
    async fn run(self) {
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
        let mut observer = MarkObserver::new();
        let mut screen_state = ScreenState::new();
        let mut prompts = 0_u64;
        let mut subscribers = 0_usize;
        let mut answered_through = Sequence(0);
        let mut requests_open = true;
        let mut exit_open = true;
        let mut ending: Option<tokio::time::Instant> = None;
        loop {
            let deadline = ending.unwrap_or_else(tokio::time::Instant::now);
            // Requests before output: a resize queued before the child was
            // told of it reaches the mirror before anything the child drew
            // for it. The drain's end before output too, so a job that
            // never stops printing cannot hold an ended pane open.
            tokio::select! {
                biased;
                request = requests.recv(), if requests_open => match request {
                    Some(Request::Screen(reply)) => {
                        let sequence = newest_of(&history);
                        let _sent = reply.send(screen_state.serialize(&mirror, sequence));
                    }
                    Some(Request::Resize { columns: width, rows: height }) => {
                        mirror.resize(width, height);
                        publish(state, &history, &mirror, false, prompts);
                    }
                    Some(Request::Subscribe { screen, reply }) => {
                        subscribers = subscribers.saturating_add(1);
                        mirror.set_subscriber_count(subscribers);
                        let begun = begin(&mirror, &screen_state, &history, screen, answered_through);
                        let _sent = reply.send(begun);
                    }
                    Some(Request::Unsubscribe) => {
                        subscribers = subscribers.saturating_sub(1);
                        mirror.set_subscriber_count(subscribers);
                    }
                    None => requests_open = false,
                },
                changed = exit.changed(), if exit_open && ending.is_none() => {
                    if changed.is_err() {
                        exit_open = false;
                    } else if exit.borrow_and_update().is_some() {
                        ending = tokio::time::Instant::now().checked_add(drain);
                        if ending.is_none() {
                            break;
                        }
                    }
                }
                () = tokio::time::sleep_until(deadline), if ending.is_some() => break,
                chunk = output.next() => match chunk {
                    Some(bytes) => {
                        let prompted = feed_chunk(
                            &bytes,
                            &mut mirror,
                            &mut observer,
                            &mut screen_state,
                            &history,
                            &marks,
                            &responses,
                        );
                        prompts = prompts.saturating_add(prompted);
                        if subscribers == 0 {
                            answered_through = newest_of(&history);
                        }
                        publish(state, &history, &mirror, false, prompts);
                    }
                    None => break,
                },
            }
        }
        publish(state, &history, &mirror, true, prompts);
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

/// Waits for the child from the moment it is spawned, on a thread of its own,
/// and publishes how it ended.
///
/// The wait does not follow the output: a shell that exits leaving a
/// background job holding its terminal never sees its output close, and a
/// reaper that waited for that would leave it a zombie and its pane alive for
/// as long as the job ran. The wait holds no terminal-owner mutex, so close
/// and drop can still signal the child while it is pending, and the thread
/// holds the owner itself until the child is reaped, so the owner's drop
/// never has to wait for it. A thread rather than the blocking pool, because
/// the wait lasts as long as the pane and the pool is shared and bounded.
///
/// # Errors
///
/// [`PaneError::Pty`] when the thread cannot be started.
fn reap_on_exit(
    process: Arc<Mutex<PtyProcess>>,
    exit: watch::Sender<Option<ExitStatus>>,
) -> Result<(), PaneError> {
    let reaper = process
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .reaper();
    std::thread::Builder::new()
        .name("pty-reaper".to_owned())
        .spawn(move || {
            let status = reaper.wait().ok();
            drop(process);
            let _sent = exit.send(status);
        })
        .map_err(|source| {
            PaneError::Pty(PtyError::Wait {
                source: source.into(),
            })
        })?;
    Ok(())
}
