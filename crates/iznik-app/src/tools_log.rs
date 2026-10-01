//! The process log the developer tools window shows.
//!
//! A launched application has no terminal. This keeps a bounded record of
//! what the process logs — the engine's `tracing` lines, once [`install`]
//! has run, and anything passed to [`record`] — in the order it was said.
//! The same bytes are written to standard error, so a launch from a terminal
//! still prints them.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock, PoisonError};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriter;

use crate::wake::WakeSignal;

/// How many complete lines the window keeps. Older lines are dropped so a
/// long session cannot grow this record without a bound.
pub const KEPT_LINES: usize = 1_000;

/// The environment variable the level is read from, the same name the daemon uses.
const LEVEL_VARIABLE: &str = "IZNIK_LOG";
/// The level when [`LEVEL_VARIABLE`] is unset.
const DEFAULT_LEVEL: &str = "info";

/// The lines kept for the window, oldest at the front, and a fragment waiting
/// for its newline.
struct Record {
    /// Complete lines.
    lines: VecDeque<String>,
    /// Bytes received since the last newline.
    partial: String,
}

impl Record {
    /// An empty record.
    const fn empty() -> Self {
        Self {
            lines: VecDeque::new(),
            partial: String::new(),
        }
    }

    /// Append `bytes`, storing each newline-terminated piece as one line.
    fn push_bytes(&mut self, bytes: &[u8]) -> bool {
        let text = String::from_utf8_lossy(bytes);
        let mut rest = text.as_ref();
        let mut stored = false;
        while let Some(end) = rest.find('\n') {
            let (head, tail) = rest.split_at(end);
            self.partial.push_str(head);
            self.keep();
            stored = true;
            rest = tail.get(1..).unwrap_or("");
        }
        self.partial.push_str(rest);
        stored
    }

    /// Store the partial line, dropping the oldest past [`KEPT_LINES`].
    fn keep(&mut self) {
        if self.partial.ends_with('\r') {
            self.partial.pop();
        }
        if self.lines.len() == KEPT_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(std::mem::take(&mut self.partial));
    }
}

/// The process-wide log.
static RECORD: Mutex<Record> = Mutex::new(Record::empty());

/// Wakes the developer tools window after a line is stored.
fn signal() -> WakeSignal {
    static WAKE: OnceLock<WakeSignal> = OnceLock::new();
    WAKE.get_or_init(WakeSignal::new).clone()
}

/// Install the process log: `tracing` lines at `IZNIK_LOG`, or `info`.
///
/// A subscriber already installed by something else is left in place. This
/// application does not ask the engine for a log file, so the subscriber
/// installed here is the one that receives the engine's lines.
pub fn install() {
    let filter = EnvFilter::try_from_env(LEVEL_VARIABLE)
        .unwrap_or_else(|_unset| EnvFilter::new(DEFAULT_LEVEL));
    let _installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(LogWriter)
        .try_init();
}

/// Record `text` as log lines, and write it to standard error.
pub fn record(text: &str) {
    let line = normalized(text);
    accept(line.as_bytes());
    let _written = io::stderr().write_all(line.as_bytes());
}

/// The stored lines, oldest first, including a fragment still waiting for its newline.
#[must_use]
pub fn snapshot() -> Vec<String> {
    let held = RECORD.lock().unwrap_or_else(PoisonError::into_inner);
    let mut lines: Vec<String> = held.lines.iter().cloned().collect();
    if !held.partial.is_empty() {
        lines.push(held.partial.clone());
    }
    lines
}

/// The signal raised after a complete line is stored.
#[must_use]
pub fn wake() -> WakeSignal {
    signal()
}

/// Append `bytes` to the process log and wake a window that is waiting.
fn accept(bytes: &[u8]) {
    let stored = {
        let mut held = RECORD.lock().unwrap_or_else(PoisonError::into_inner);
        held.push_bytes(bytes)
    };
    if stored {
        signal().raise();
    }
}

/// `text` with a trailing newline, so every record ends a line.
fn normalized(text: &str) -> String {
    let mut line = text.to_owned();
    if !line.ends_with('\n') {
        line.push('\n');
    }
    line
}

/// The `tracing` writer. The lines live in [`RECORD`].
#[derive(Clone, Copy, Debug)]
struct LogWriter;

/// One event's writer.
#[derive(Debug)]
struct LogSink;

impl<'writer> MakeWriter<'writer> for LogWriter {
    type Writer = LogSink;

    fn make_writer(&'writer self) -> Self::Writer {
        LogSink
    }
}

impl Write for LogSink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        accept(buffer);
        let _written = io::stderr().write(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}
