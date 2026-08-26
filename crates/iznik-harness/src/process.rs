//! The one place a child process is spawned synchronously: in its own process
//! group, under a deadline that ends the whole group and reports what the
//! child said. `xtask`, the fixture and the scenario runner all go through
//! here, so nothing in this repository waits on a child without a bound.
//!
//! Standard input is whatever the caller configured on the command, inherited
//! by default; a caller that must feed a child hands it a file.

use std::fmt::{self, Display, Formatter};
use std::io::{self, ErrorKind, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// How long a process group is given to end after `SIGTERM` before it is sent
/// `SIGKILL`: two seconds, enough for a build tool to flush what it was writing
/// and negligible beside a deadline measured in minutes.
pub const TERMINATION_GRACE: Duration = Duration::from_secs(2);

/// The most of a child's standard output or error that [`Output::Capture`]
/// keeps, per stream: one mebibyte, the tail of a flood being what a person
/// reads and a test asserts on.
pub const CAPTURE_LIMIT_BYTES: usize = 1 << 20;

/// How often a running child is checked for its exit and against its
/// deadline: ten milliseconds, coarse enough to cost nothing and fine enough
/// that a deadline is honoured within it.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long, after the child has exited, its captured streams are given to
/// reach end of file before what has arrived is taken: a hundred
/// milliseconds, orders of magnitude more than draining a pipe takes, and
/// the bound that keeps a grandchild holding the pipe open from holding the
/// caller.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(100);

/// The size of the buffer each capturing read fills: sixty-four kibibytes,
/// a pipe's own capacity.
const READ_BUFFER_LENGTH: usize = 64 * 1024;

/// The factor by which a captured stream may exceed its limit before it is
/// trimmed back to the limit, so that a flood costs one move per limit of
/// bytes rather than one per read.
const TRIM_FACTOR: usize = 2;

/// The statement appended to a captured stream that was still open when the
/// drain grace passed: what came after it, if anything, is not here.
const INCOMPLETE_STATEMENT: &str =
    "\n[the stream was still open after the child exited; its output may be incomplete]\n";

/// How long a child may run before its process group is ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline(pub Duration);

/// Where a child's standard output and error go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Through to this process's own streams as they happen — what a gate
    /// needs, because an idle watchdog reads that stream and a gate must never
    /// buffer ten minutes of output to print it at the end.
    Inherit,
    /// Into memory, each stream capped at [`CAPTURE_LIMIT_BYTES`] with its
    /// tail kept and the truncation stated — for tests and for callers that
    /// parse what the child said.
    Capture,
}

/// A child that exited successfully within its deadline.
#[derive(Debug)]
pub struct Completed {
    /// Always success: a child that exits otherwise is
    /// [`ProcessError::Failed`].
    pub status: ExitStatus,
    /// The standard output, empty under [`Output::Inherit`].
    pub stdout: Vec<u8>,
    /// The standard error, empty under [`Output::Inherit`].
    pub stderr: Vec<u8>,
    /// From spawn to exit.
    pub elapsed: Duration,
}

/// Why a child did not complete; every variant names the program.
#[derive(Debug)]
pub enum ProcessError {
    /// The program could not be started.
    Spawn {
        /// The program as the command named it.
        program: String,
        /// What the operating system said.
        source: io::Error,
    },
    /// The deadline passed: the process group was ended, and the tails are
    /// what the child said before that — empty under [`Output::Inherit`],
    /// which had already streamed it through.
    TimedOut {
        /// The program as the command named it.
        program: String,
        /// The deadline that passed.
        deadline: Duration,
        /// The end of the standard output, as text.
        stdout_tail: String,
        /// The end of the standard error, as text.
        stderr_tail: String,
    },
    /// The child exited with a status other than success.
    Failed {
        /// The program as the command named it.
        program: String,
        /// The status it exited with.
        status: ExitStatus,
        /// The end of the standard error, as text.
        stderr_tail: String,
    },
    /// The child could not be watched to its end: its exit could not be
    /// observed, its output could not be read, or its group could not be
    /// signalled.
    Wait {
        /// The program as the command named it.
        program: String,
        /// What the operating system said.
        source: io::Error,
    },
}

impl Display for ProcessError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ProcessError::Spawn { program, source } => {
                write!(formatter, "{program}: could not be started: {source}")
            }
            ProcessError::TimedOut {
                program,
                deadline,
                stdout_tail,
                stderr_tail,
            } => {
                write!(
                    formatter,
                    "{program}: did not finish within {deadline:?} and was ended"
                )?;
                write_tail(formatter, "stdout", stdout_tail)?;
                write_tail(formatter, "stderr", stderr_tail)
            }
            ProcessError::Failed {
                program,
                status,
                stderr_tail,
            } => {
                write!(formatter, "{program}: {status}")?;
                write_tail(formatter, "stderr", stderr_tail)
            }
            ProcessError::Wait { program, source } => {
                write!(
                    formatter,
                    "{program}: could not be watched to its end: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProcessError::Spawn { source, .. } | ProcessError::Wait { source, .. } => Some(source),
            ProcessError::TimedOut { .. } | ProcessError::Failed { .. } => None,
        }
    }
}

/// Appends a named, non-empty tail to an error's text on its own lines.
///
/// # Errors
///
/// When the formatter does.
fn write_tail(formatter: &mut Formatter<'_>, stream: &str, tail: &str) -> fmt::Result {
    if tail.is_empty() {
        return Ok(());
    }
    write!(formatter, "\n--- {stream} tail ---\n{}", tail.trim_end())
}

/// Runs a command to completion under a deadline.
///
/// The child is spawned in its own process group. Its exit is polled; when
/// the deadline passes, the group is sent `SIGTERM`, given
/// [`TERMINATION_GRACE`] to empty, and sent `SIGKILL` if it has not, so
/// neither the child nor a grandchild survives the deadline. Under
/// [`Output::Capture`] each stream is read on a thread of its own into a
/// capped buffer.
///
/// # Errors
///
/// [`ProcessError::Spawn`] when the program cannot be started,
/// [`ProcessError::TimedOut`] when the deadline passes,
/// [`ProcessError::Failed`] when the child exits with anything but success,
/// and [`ProcessError::Wait`] when it cannot be watched to its end.
pub fn run(
    mut command: Command,
    deadline: Deadline,
    output: Output,
) -> Result<Completed, ProcessError> {
    let program = command.get_program().to_string_lossy().into_owned();
    command.process_group(0);
    let (stdout_mode, stderr_mode) = match output {
        Output::Inherit => (Stdio::inherit(), Stdio::inherit()),
        Output::Capture => (Stdio::piped(), Stdio::piped()),
    };
    command.stdout(stdout_mode).stderr(stderr_mode);
    let started = Instant::now();
    let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
        program: program.clone(),
        source,
    })?;
    let group = match process_group(&child, &program) {
        Ok(group) => group,
        Err(error) => {
            child.kill().unwrap_or_default();
            child.wait().map(|_status| ()).unwrap_or_default();
            return Err(error);
        }
    };
    let streams = match capture_both(&mut child) {
        Ok(streams) => streams,
        Err(source) => {
            end_process_group(&mut child, group, &program)?;
            return Err(ProcessError::Wait { program, source });
        }
    };
    let Some(status) = await_exit(&mut child, &program, started, deadline)? else {
        end_process_group(&mut child, group, &program)?;
        let (stdout, stderr) = streams.drain();
        return Err(ProcessError::TimedOut {
            program,
            deadline: deadline.0,
            stdout_tail: text(&stdout),
            stderr_tail: text(&stderr),
        });
    };
    let elapsed = started.elapsed();
    let (stdout, stderr) = streams.drain();
    if !status.success() {
        return Err(ProcessError::Failed {
            program,
            status,
            stderr_tail: text(&stderr),
        });
    }
    Ok(Completed {
        status,
        stdout,
        stderr,
        elapsed,
    })
}

/// The child's process group, which is its own process id.
///
/// # Errors
///
/// [`ProcessError::Wait`] when the id is not representable as a group — which
/// no Linux hands out, and which must never fall back to group zero, this
/// process's own.
fn process_group(child: &Child, program: &str) -> Result<Pid, ProcessError> {
    i32::try_from(child.id())
        .map(Pid::from_raw)
        .map_err(|_error| ProcessError::Wait {
            program: program.to_owned(),
            source: io::Error::other("the child's process id is not representable"),
        })
}

/// Polls the child until it exits — `Some(status)` — or the deadline passes
/// — `None`, with the child still running.
///
/// # Errors
///
/// [`ProcessError::Wait`] when the child's exit cannot be observed.
fn await_exit(
    child: &mut Child,
    program: &str,
    started: Instant,
    deadline: Deadline,
) -> Result<Option<ExitStatus>, ProcessError> {
    loop {
        if let Some(status) = try_wait(child, program)? {
            return Ok(Some(status));
        }
        if started.elapsed() >= deadline.0 {
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// One non-blocking check of the child's exit.
///
/// # Errors
///
/// [`ProcessError::Wait`] when the check itself fails.
fn try_wait(child: &mut Child, program: &str) -> Result<Option<ExitStatus>, ProcessError> {
    child.try_wait().map_err(|source| ProcessError::Wait {
        program: program.to_owned(),
        source,
    })
}

/// Whether nothing is left in the group: a null signal finds no process.
fn group_is_empty(group: Pid) -> bool {
    matches!(killpg(group, None), Err(Errno::ESRCH))
}

/// Ends the child's process group: `SIGTERM`; then, for the grace, wait for
/// the child to be reaped and the group to empty; then `SIGKILL` whatever
/// remains, and reap the child. `SIGKILL` is sent only after the group was
/// found non-empty a moment before, so a recycled id would have to arrive
/// within one syscall of that check. A `SIGTERM` that finds no group is a
/// group that ended on its own; a `SIGKILL` refused for any reason but that
/// would leave the wait unbounded, so it is an error.
///
/// # Errors
///
/// [`ProcessError::Wait`] when the child cannot be reaped or the group cannot
/// be killed.
fn end_process_group(child: &mut Child, group: Pid, program: &str) -> Result<(), ProcessError> {
    killpg(group, Signal::SIGTERM).unwrap_or_default();
    let terminated = Instant::now();
    let mut reaped = false;
    loop {
        if !reaped && try_wait(child, program)?.is_some() {
            reaped = true;
        }
        if reaped && group_is_empty(group) {
            return Ok(());
        }
        if terminated.elapsed() >= TERMINATION_GRACE {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    if reaped && group_is_empty(group) {
        return Ok(());
    }
    match killpg(group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(errno) => {
            return Err(ProcessError::Wait {
                program: program.to_owned(),
                source: io::Error::from(errno),
            });
        }
    }
    if !reaped {
        child.wait().map_err(|source| ProcessError::Wait {
            program: program.to_owned(),
            source,
        })?;
    }
    Ok(())
}

/// The last [`CAPTURE_LIMIT_BYTES`] of a stream, how many bytes before them
/// were dropped, and whether the stream has reached its end.
#[derive(Debug, Default)]
struct Tail {
    /// The bytes kept, at most [`TRIM_FACTOR`] limits between trims.
    kept: Vec<u8>,
    /// How many bytes were dropped from the front.
    dropped: usize,
    /// Whether the reader reached end of file or gave up.
    finished: bool,
}

impl Tail {
    /// Appends bytes, trimming the front once the buffer exceeds
    /// [`TRIM_FACTOR`] limits.
    fn push(&mut self, bytes: &[u8]) {
        self.kept.extend_from_slice(bytes);
        if self.kept.len() > CAPTURE_LIMIT_BYTES.saturating_mul(TRIM_FACTOR) {
            self.trim();
        }
    }

    /// Drops everything before the last limit of bytes.
    fn trim(&mut self) {
        let excess = self.kept.len().saturating_sub(CAPTURE_LIMIT_BYTES);
        if excess == 0 {
            return;
        }
        self.kept.drain(..excess);
        self.dropped = self.dropped.saturating_add(excess);
    }

    /// The captured bytes: the tail, preceded by a statement of the truncation
    /// when anything was dropped, and followed by one when the stream was
    /// still open.
    fn into_bytes(mut self) -> Vec<u8> {
        self.trim();
        let mut bytes = if self.dropped == 0 {
            Vec::new()
        } else {
            format!("[{} bytes dropped before this tail]\n", self.dropped).into_bytes()
        };
        bytes.extend_from_slice(&self.kept);
        if !self.finished {
            bytes.extend_from_slice(INCOMPLETE_STATEMENT.as_bytes());
        }
        bytes
    }
}

/// The two captured streams of a child, or nothing under
/// [`Output::Inherit`].
#[derive(Debug)]
struct Streams {
    /// The standard output's tail, when captured.
    stdout: Option<Arc<Mutex<Tail>>>,
    /// The standard error's tail, when captured.
    stderr: Option<Arc<Mutex<Tail>>>,
}

impl Streams {
    /// What the two streams hold once the child has exited.
    fn drain(self) -> (Vec<u8>, Vec<u8>) {
        (drain(self.stdout), drain(self.stderr))
    }
}

/// Starts a reader for each piped stream the child has.
///
/// # Errors
///
/// When a reader thread cannot be started.
fn capture_both(child: &mut Child) -> io::Result<Streams> {
    let stdout = child.stdout.take().map(capture).transpose()?;
    let stderr = child.stderr.take().map(capture).transpose()?;
    Ok(Streams { stdout, stderr })
}

/// Reads a stream to its end on a thread of its own, into a shared tail; the
/// thread is not joined, so a stream a grandchild keeps open never holds the
/// caller.
///
/// # Errors
///
/// When the thread cannot be started.
fn capture<Reader: Read + Send + 'static>(mut reader: Reader) -> io::Result<Arc<Mutex<Tail>>> {
    let tail = Arc::new(Mutex::new(Tail::default()));
    let shared = Arc::clone(&tail);
    thread::Builder::new()
        .name("iznik-capture".to_owned())
        .spawn(move || {
            let mut buffer = vec![0; READ_BUFFER_LENGTH];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
                        guard.push(buffer.get(..count).unwrap_or_default());
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finished = true;
        })?;
    Ok(tail)
}

/// What a captured stream holds once the child has exited: the reader is
/// given [`OUTPUT_DRAIN_GRACE`] to reach the end, and what has arrived by
/// then is taken, with a statement when the end had not come — a grandchild
/// writing after the child's exit is not the child.
fn drain(tail: Option<Arc<Mutex<Tail>>>) -> Vec<u8> {
    let Some(tail) = tail else {
        return Vec::new();
    };
    let started = Instant::now();
    loop {
        let finished = tail.lock().unwrap_or_else(PoisonError::into_inner).finished;
        if finished || started.elapsed() >= OUTPUT_DRAIN_GRACE {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let mut guard = tail.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut *guard).into_bytes()
}

/// Captured bytes as text, with lossy replacement.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
