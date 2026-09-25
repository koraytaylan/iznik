//! The one module where blocking I/O exists: dedicated threads that turn the
//! pseudoterminal descriptor into an async output stream and an input queue.
//!
//! The output reader blocks on a bounded channel, so a consumer that falls
//! behind stalls the reader, which stalls the child's writes — the backpressure
//! a slow terminal applies, confined to one pane. The input writer writes each
//! submitted message whole before taking the next, so bytes submitted in one
//! call are never interleaved with another caller's; a paste into a stalled
//! program backs up to a cap, and a caller that finds no room is told how to
//! wait for some rather than having anything dropped.

use std::fmt::{self, Display, Formatter};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc::{Receiver as AsyncReceiver, Sender as AsyncSender};
use tokio::sync::watch;

use crate::pty::spawn::{PtyError, PtyProcess};

/// The most one output chunk carries: a full read of the descriptor.
pub const READ_CHUNK_LENGTH: usize = 64 * 1024;

/// How many chunks the output channel holds before the reader must wait — the
/// backpressure a slow consumer applies to its own child.
pub const OUTPUT_CHANNEL_CHUNKS: usize = 16;

/// The most input that may wait to be written before a further write is
/// refused as a backlog rather than dropped. One write is always taken when
/// nothing is waiting, however long it is, so a write never waits for room
/// that cannot come.
pub const MAXIMUM_PENDING_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// How long the reader waits in `poll`, and how often it looks at a pause.
///
/// Short enough that a replacement stops consuming the terminal before it
/// copies history, and long enough that an idle pane does not wake constantly.
const READER_WAIT: Duration = Duration::from_millis(20);

/// The async stream of a pane's output, in chunks of at most
/// [`READ_CHUNK_LENGTH`], ending when the child's terminal closes.
#[derive(Debug)]
pub struct OutputStream {
    /// Chunks from the reader thread; closed when it ends.
    chunks: AsyncReceiver<Vec<u8>>,
    /// Set while a replacement wants the reader to leave the terminal alone.
    hold: Arc<AtomicBool>,
    /// Whether the reader has stopped taking bytes, so the held ones can be copied.
    settled: watch::Receiver<bool>,
}

impl OutputStream {
    /// The next chunk of output, or `None` once the child's terminal has closed
    /// and every chunk before it has been taken.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.chunks.recv().await
    }

    /// A chunk already read, or `None` when nothing is waiting.
    ///
    /// Does not wait. A replacement drains these after the reader has stopped,
    /// so a byte the reader already took still reaches history.
    pub fn try_next(&mut self) -> Option<Vec<u8>> {
        self.chunks.try_recv().ok()
    }

    /// Stops the reader from taking any further byte, and waits until it has.
    ///
    /// Bytes still in the kernel stay there. Bytes already queued are left for
    /// [`Self::try_next`]. [`Self::resume`] lets the reader continue, which a
    /// replacement does when the new binary cannot be executed.
    pub async fn pause(&mut self) {
        self.hold.store(true, Ordering::Release);
        loop {
            if *self.settled.borrow() {
                return;
            }
            if self.settled.changed().await.is_err() {
                return;
            }
        }
    }

    /// Lets the reader take bytes again after [`Self::pause`].
    pub fn resume(&self) {
        self.hold.store(false, Ordering::Release);
    }
}

/// A handle to a pane's input: each `write` is one message the writer thread
/// puts through whole.
#[derive(Clone, Debug)]
pub struct InputHandle {
    /// Messages for the writer thread.
    messages: Sender<Vec<u8>>,
    /// The bytes now waiting to be written, reserved on submit and released
    /// once written, so a backlog is refused before it grows without bound.
    pending: Arc<AtomicUsize>,
    /// Told each time the writer releases bytes or ends, so a caller that
    /// found no room can wait for some.
    released: Arc<watch::Sender<()>>,
    /// Whether the writer has ended, after which nothing is written.
    closed: Arc<AtomicBool>,
}

/// What became of input offered to a pane.
#[derive(Debug)]
pub enum Offer {
    /// It was enqueued, to be written whole.
    Taken,
    /// There was no room for it; nothing was enqueued. The bytes come back,
    /// with the way to wait until offering them again may find room.
    Full {
        /// The bytes offered, untouched.
        bytes: Vec<u8>,
        /// Resolves once the writer has released some bytes, or ended.
        room: InputRoom,
    },
}

/// A wait for a pane's input to make room.
#[derive(Debug)]
pub struct InputRoom(watch::Receiver<()>);

impl InputRoom {
    /// Resolves once the writer has released some bytes or ended since the
    /// offer that handed this out — at once if it already has.
    pub async fn wait(&mut self) {
        // A writer gone for good is an answer too: the next offer says so.
        let _changed = self.0.changed().await;
    }
}

/// Why input could not be accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputError {
    /// The pending input would exceed [`MAXIMUM_PENDING_INPUT_BYTES`]; nothing
    /// was enqueued, and nothing was dropped.
    Backlog {
        /// How much was already waiting.
        pending: usize,
    },
    /// The child's terminal has closed; there is nothing to write to. Beyond the
    /// architecture's named `Backlog`, because a write after the child has ended
    /// must be refused, not silently dropped nor misreported as a backlog.
    Closed,
}

impl Display for InputError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            InputError::Backlog { pending } => {
                write!(formatter, "input backed up: {pending} bytes are waiting")
            }
            InputError::Closed => write!(formatter, "the child's terminal has closed"),
        }
    }
}

impl std::error::Error for InputError {}

impl InputHandle {
    /// Enqueues one message to be written whole.
    ///
    /// # Errors
    ///
    /// [`InputError::Backlog`] when accepting it would pass
    /// [`MAXIMUM_PENDING_INPUT_BYTES`], and [`InputError::Closed`] when the
    /// child's terminal has closed.
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), InputError> {
        self.write_reserved(bytes)
            .map_err(|(refusal, _bytes)| refusal)
    }

    /// Enqueues one message to be written whole if there is room for it, and
    /// otherwise hands it back with the way to wait for room: input a caller
    /// has to deliver, in order, is held rather than refused.
    ///
    /// # Errors
    ///
    /// [`InputError::Closed`] when the child's terminal has closed.
    pub fn offer(&self, bytes: Vec<u8>) -> Result<Offer, InputError> {
        // Subscribed before the budget is looked at, so a release between the
        // two still wakes the wait.
        let room = InputRoom(self.released.subscribe());
        if self.closed.load(Ordering::Acquire) {
            return Err(InputError::Closed);
        }
        match self.write_reserved(bytes) {
            Ok(()) => Ok(Offer::Taken),
            Err((InputError::Backlog { .. }, bytes)) => Ok(Offer::Full { bytes, room }),
            Err((refusal, _bytes)) => Err(refusal),
        }
    }

    /// Reserves room for `bytes` and enqueues them, or hands them back with
    /// the refusal.
    ///
    /// # Errors
    ///
    /// [`InputError::Backlog`] and [`InputError::Closed`] as [`Self::write`].
    fn write_reserved(&self, bytes: Vec<u8>) -> Result<(), (InputError, Vec<u8>)> {
        let length = bytes.len();
        if let Err(refusal) = self.reserve(length) {
            return Err((refusal, bytes));
        }
        if let Err(unsent) = self.messages.send(bytes) {
            self.pending.fetch_sub(length, Ordering::AcqRel);
            return Err((InputError::Closed, unsent.0));
        }
        Ok(())
    }

    /// Reserves `length` bytes of the pending budget, or refuses the write.
    ///
    /// # Errors
    ///
    /// [`InputError::Backlog`] when the budget cannot hold `length` more.
    fn reserve(&self, length: usize) -> Result<(), InputError> {
        let mut pending = self.pending.load(Ordering::Acquire);
        loop {
            let next = pending.saturating_add(length);
            if pending > 0 && next > MAXIMUM_PENDING_INPUT_BYTES {
                return Err(InputError::Backlog { pending });
            }
            match self.pending.compare_exchange_weak(
                pending,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_previous) => return Ok(()),
                Err(current) => pending = current,
            }
        }
    }
}

/// Turns a process's pseudoterminal into an async output stream and an input
/// handle, each served by its own blocking thread.
///
/// # Errors
///
/// [`PtyError::Open`] when the master's reader cannot be cloned or its writer
/// cannot be taken.
pub fn streams(process: &PtyProcess) -> Result<(OutputStream, InputHandle), PtyError> {
    let writer = process
        .master()
        .take_writer()
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    let (chunk_sender, chunks) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CHUNKS);
    let hold = Arc::new(AtomicBool::new(false));
    let (settled_sender, settled) = watch::channel(false);
    let holding = Arc::clone(&hold);
    // The threads are detached — each ends itself when the terminal closes or its
    // channel drops — but a spawn that fails is an open-time failure to report,
    // not a silently dead pane.
    spawn_reader(process, chunk_sender, holding, settled_sender)?;
    let (messages, queue) = channel();
    let input = InputHandle {
        messages,
        pending: Arc::new(AtomicUsize::new(0)),
        released: Arc::new(watch::Sender::new(())),
        closed: Arc::new(AtomicBool::new(false)),
    };
    let writing = Writing {
        pending: Arc::clone(&input.pending),
        released: Arc::clone(&input.released),
        closed: Arc::clone(&input.closed),
    };
    thread::Builder::new()
        .name("pty-input".to_owned())
        .spawn(move || write_input(writer, &queue, &writing))
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    Ok((
        OutputStream {
            chunks,
            hold,
            settled,
        },
        input,
    ))
}

/// Starts the output thread on a duplicate of the master.
///
/// # Errors
///
/// [`PtyError::Open`] when the master cannot be duplicated or the thread
/// cannot be started.
fn spawn_reader(
    process: &PtyProcess,
    chunks: AsyncSender<Vec<u8>>,
    hold: Arc<AtomicBool>,
    settled: watch::Sender<bool>,
) -> Result<(), PtyError> {
    #[cfg(unix)]
    let reader = duplicate_master(process)?;
    #[cfg(not(unix))]
    let reader = process
        .master()
        .try_clone_reader()
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    thread::Builder::new()
        .name("pty-output".to_owned())
        .spawn(move || {
            #[cfg(unix)]
            read_output_waiting(reader, &chunks, &hold, &settled);
            #[cfg(not(unix))]
            {
                let _hold = hold;
                let _settled = settled;
                read_output_blocking(reader, &chunks);
            }
        })
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    Ok(())
}

/// A close-on-exec duplicate of the master, so the reader can poll it.
///
/// # Errors
///
/// [`PtyError::Open`] when the master has no descriptor or cannot be duplicated.
#[cfg(unix)]
fn duplicate_master(process: &PtyProcess) -> Result<std::fs::File, PtyError> {
    let descriptor = process.master().as_raw_fd().ok_or_else(|| PtyError::Open {
        source: "the master has no descriptor".into(),
    })?;
    let owned = iznik::duplicate_descriptor(descriptor).map_err(|error| PtyError::Open {
        source: error.into(),
    })?;
    Ok(std::fs::File::from(owned))
}

/// Reads until the terminal closes, without a way to pause.
#[cfg(not(unix))]
fn read_output_blocking(mut reader: Box<dyn Read + Send>, chunks: &AsyncSender<Vec<u8>>) {
    let mut buffer = vec![0; READ_CHUNK_LENGTH];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                let chunk = buffer.get(..count).unwrap_or_default().to_vec();
                if chunks.blocking_send(chunk).is_err() {
                    return;
                }
            }
        }
    }
}

/// Polls `reader` so [`OutputStream::pause`] can stop it between reads.
#[cfg(unix)]
fn read_output_waiting(
    mut reader: std::fs::File,
    chunks: &AsyncSender<Vec<u8>>,
    hold: &AtomicBool,
    settled: &watch::Sender<bool>,
) {
    use std::os::fd::AsFd;
    let mut buffer = vec![0; READ_CHUNK_LENGTH];
    let timeout =
        nix::poll::PollTimeout::try_from(READER_WAIT).unwrap_or(nix::poll::PollTimeout::ZERO);
    loop {
        if hold.load(Ordering::Acquire) {
            let _paused = settled.send(true);
            while hold.load(Ordering::Acquire) {
                thread::sleep(READER_WAIT);
            }
            let _resumed = settled.send(false);
            continue;
        }
        let mut waiting = [nix::poll::PollFd::new(
            reader.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        let _polled = nix::poll::poll(&mut waiting, timeout);
        if hold.load(Ordering::Acquire) {
            continue;
        }
        if !waiting
            .first()
            .and_then(nix::poll::PollFd::any)
            .unwrap_or(false)
        {
            continue;
        }
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Err(error) if terminal_closed(&error) => return,
            Err(_error) => return,
            Ok(count) => {
                let chunk = buffer.get(..count).unwrap_or_default().to_vec();
                if chunks.blocking_send(chunk).is_err() {
                    return;
                }
            }
        }
    }
}

/// Whether `error` is the pseudoterminal's end-of-file.
///
/// A read of a master whose slave has closed fails with `EIO` rather than
/// returning zero. That is the reader ending, not a pause.
#[cfg(unix)]
fn terminal_closed(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| nix::errno::Errno::from_raw(code) == nix::errno::Errno::EIO)
}

/// What the input thread shares with the handles that feed it.
struct Writing {
    /// The bytes waiting to be written.
    pending: Arc<AtomicUsize>,
    /// Told whenever bytes are released, and when the thread ends.
    released: Arc<watch::Sender<()>>,
    /// Set when the thread ends.
    closed: Arc<AtomicBool>,
}

impl Drop for Writing {
    fn drop(&mut self) {
        // However the thread ends, whoever waits for room is told, and finds
        // the input closed rather than waiting for ever.
        self.closed.store(true, Ordering::Release);
        self.released.send_replace(());
    }
}

/// The input thread: each message written whole before the next, its bytes
/// released from the pending budget once written.
fn write_input(mut writer: Box<dyn Write + Send>, queue: &Receiver<Vec<u8>>, writing: &Writing) {
    while let Ok(message) = queue.recv() {
        let length = message.len();
        let written = writer.write_all(&message).and_then(|()| writer.flush());
        writing.pending.fetch_sub(length, Ordering::AcqRel);
        writing.released.send_replace(());
        if written.is_err() {
            // The child's terminal is gone. Any messages still queued keep their
            // reservation, but `pending` stays capped and is freed with the
            // handle; a later write is refused, never dropped or double-written.
            return;
        }
    }
}
