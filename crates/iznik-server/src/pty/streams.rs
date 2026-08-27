//! The one module where blocking I/O exists: dedicated threads that turn the
//! pseudoterminal descriptor into an async output stream and an input queue.
//!
//! The output reader blocks on a bounded channel, so a consumer that falls
//! behind stalls the reader, which stalls the child's writes — the backpressure
//! a slow terminal applies, confined to one pane. The input writer writes each
//! submitted message whole before taking the next, so bytes submitted in one
//! call are never interleaved with another caller's; a paste into a stalled
//! program backs up to a cap and is refused there, never dropped.

use std::fmt::{self, Display, Formatter};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use tokio::sync::mpsc::{Receiver as AsyncReceiver, Sender as AsyncSender};

use crate::pty::spawn::{PtyError, PtyProcess};

/// The most one output chunk carries: a full read of the descriptor.
pub const READ_CHUNK_LENGTH: usize = 64 * 1024;

/// How many chunks the output channel holds before the reader must wait — the
/// backpressure a slow consumer applies to its own child.
pub const OUTPUT_CHANNEL_CHUNKS: usize = 16;

/// The most input that may wait to be written before a further write is
/// refused as a backlog rather than dropped.
pub const MAXIMUM_PENDING_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// The async stream of a pane's output, in chunks of at most
/// [`READ_CHUNK_LENGTH`], ending when the child's terminal closes.
#[derive(Debug)]
pub struct OutputStream {
    /// Chunks from the reader thread; closed when it ends.
    chunks: AsyncReceiver<Vec<u8>>,
}

impl OutputStream {
    /// The next chunk of output, or `None` once the child's terminal has closed
    /// and every chunk before it has been taken.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.chunks.recv().await
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
        let length = bytes.len();
        self.reserve(length)?;
        if self.messages.send(bytes).is_err() {
            self.pending.fetch_sub(length, Ordering::AcqRel);
            return Err(InputError::Closed);
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
            if next > MAXIMUM_PENDING_INPUT_BYTES {
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
    let reader = process
        .master()
        .try_clone_reader()
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    let writer = process
        .master()
        .take_writer()
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    let (chunk_sender, chunks) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CHUNKS);
    // The threads are detached — each ends itself when the terminal closes or its
    // channel drops — but a spawn that fails is an open-time failure to report,
    // not a silently dead pane.
    thread::Builder::new()
        .name("pty-output".to_owned())
        .spawn(move || read_output(reader, &chunk_sender))
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    let (messages, queue) = channel();
    let pending = Arc::new(AtomicUsize::new(0));
    let writer_pending = Arc::clone(&pending);
    thread::Builder::new()
        .name("pty-input".to_owned())
        .spawn(move || write_input(writer, &queue, &writer_pending))
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    Ok((OutputStream { chunks }, InputHandle { messages, pending }))
}

/// The output thread: chunks from the descriptor to the channel, blocking when
/// it is full, until the terminal closes or nobody listens.
fn read_output(mut reader: Box<dyn Read + Send>, chunks: &AsyncSender<Vec<u8>>) {
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

/// The input thread: each message written whole before the next, its bytes
/// released from the pending budget once written.
fn write_input(
    mut writer: Box<dyn Write + Send>,
    queue: &Receiver<Vec<u8>>,
    pending: &AtomicUsize,
) {
    while let Ok(message) = queue.recv() {
        let length = message.len();
        let written = writer.write_all(&message).and_then(|()| writer.flush());
        pending.fetch_sub(length, Ordering::AcqRel);
        if written.is_err() {
            // The child's terminal is gone. Any messages still queued keep their
            // reservation, but `pending` stays capped and is freed with the
            // handle; a later write is refused, never dropped or double-written.
            return;
        }
    }
}
