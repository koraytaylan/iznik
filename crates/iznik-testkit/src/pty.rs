//! The pseudoterminal harness: real processes on real pseudoterminals, read
//! until quiet rather than until a clock. A reader thread hands the child's
//! output over a channel in chunks, so [`PtyChild::read_until_quiet`] can end
//! on silence — and a read that comes back empty and succeeds, the worst
//! failure in this domain, is impossible: a cap with nothing received is an
//! error carrying whatever was seen. Tests spawn `sh`, `cat` and small
//! scripts, never a login shell whose prompt draws itself asynchronously.

use core::fmt::{self, Display, Formatter};
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// How many bytes the reader thread asks the pseudoterminal for at once: a
/// terminal line discipline hands out at most a few kibibytes per read.
const READ_LENGTH: usize = 4096;

/// The pixel size the child is told, which nothing here measures.
const PIXEL_SIZE: u16 = 0;

/// The terminal type the child sees, whatever the developer's own is.
const TERMINAL_TYPE: &str = "xterm-256color";

/// How a child ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// It exited with this code.
    Exited(u32),
    /// A signal ended it; the name is the platform's description of the
    /// signal, as `Terminated` or `Killed`.
    Signalled(String),
}

/// Why the harness could not do what was asked.
#[derive(Debug)]
pub enum PtyError {
    /// The pseudoterminal could not be opened or the child could not start.
    Spawn {
        /// The program asked for.
        program: String,
        /// What went wrong.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Bytes could not be written to the child.
    Write {
        /// What the operating system said.
        source: io::Error,
    },
    /// The cap elapsed before the child fell quiet.
    Timeout {
        /// The bytes seen before the cap, escaped for a message.
        received: String,
    },
    /// The child closed its terminal having produced nothing; a child that
    /// produced something and then closed is a success carrying it.
    Closed {
        /// The bytes seen before the close, escaped for a message.
        received: String,
    },
    /// The terminal could not be resized.
    Resize {
        /// What went wrong.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The child could not be waited for.
    Wait {
        /// What the operating system said.
        source: io::Error,
    },
}

impl Display for PtyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            PtyError::Spawn { program, source } => {
                write!(
                    formatter,
                    "`{program}` could not be spawned on a pseudoterminal: {source}"
                )
            }
            PtyError::Write { source } => {
                write!(formatter, "the child could not be written to: {source}")
            }
            PtyError::Timeout { received } => {
                write!(
                    formatter,
                    "the cap elapsed before the child fell quiet; received `{received}`"
                )
            }
            PtyError::Closed { received } => {
                write!(
                    formatter,
                    "the child closed its terminal; received `{received}`"
                )
            }
            PtyError::Resize { source } => {
                write!(formatter, "the terminal could not be resized: {source}")
            }
            PtyError::Wait { source } => {
                write!(formatter, "the child could not be waited for: {source}")
            }
        }
    }
}

impl std::error::Error for PtyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PtyError::Spawn { source, .. } | PtyError::Resize { source } => Some(source.as_ref()),
            PtyError::Write { source } | PtyError::Wait { source } => Some(source),
            PtyError::Timeout { .. } | PtyError::Closed { .. } => None,
        }
    }
}

/// Bytes as a message shows them.
fn escaped(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

/// A child process on its own pseudoterminal.
pub struct PtyChild {
    /// The terminal's master side.
    master: Box<dyn MasterPty + Send>,
    /// Where bytes to the child go.
    writer: Box<dyn Write + Send>,
    /// The child.
    child: Box<dyn Child + Send + Sync>,
    /// The child's process id, which is also its process group.
    process_id: u32,
    /// Chunks the reader thread hands over as they arrive.
    chunks: Receiver<Vec<u8>>,
    /// Whether the child has been waited for, so the drop need not kill it.
    reaped: bool,
}

impl fmt::Debug for PtyChild {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "PtyChild({})", self.process_id)
    }
}

/// The spawn error for a program.
fn spawn_error(
    program: &str,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> PtyError {
    PtyError::Spawn {
        program: program.to_owned(),
        source: source.into(),
    }
}

impl PtyChild {
    /// Starts `program` with `arguments` on a new pseudoterminal of the
    /// given size, with `TERM` set to a fixed terminal type.
    ///
    /// # Errors
    ///
    /// [`PtyError::Spawn`] when the terminal cannot be opened, the program
    /// cannot start, or the reader thread cannot be created.
    pub fn spawn(
        program: &str,
        arguments: &[&str],
        columns: u16,
        rows: u16,
    ) -> Result<PtyChild, PtyError> {
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: PIXEL_SIZE,
                pixel_height: PIXEL_SIZE,
            })
            .map_err(|source| spawn_error(program, source))?;
        // The terminal's ends are taken before the child exists, so nothing
        // that fails after it does leaves a process behind: the child is then
        // either killed here or owned by a `PtyChild` whose drop kills it.
        let master = pair.master;
        let reader = master
            .try_clone_reader()
            .map_err(|source| spawn_error(program, source))?;
        let writer = master
            .take_writer()
            .map_err(|source| spawn_error(program, source))?;
        let mut command = CommandBuilder::new(program);
        command.args(arguments);
        command.env("TERM", TERMINAL_TYPE);
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|source| spawn_error(program, source))?;
        drop(pair.slave);
        let Some(process_id) = child.process_id() else {
            let _hung_up = child.kill();
            let _reaped = child.wait();
            return Err(spawn_error(program, "the child has no process id"));
        };
        let (sender, chunks) = mpsc::channel();
        let started = PtyChild {
            master,
            writer,
            child,
            process_id,
            chunks,
            reaped: false,
        };
        thread::Builder::new()
            .name(format!("pty-reader-{process_id}"))
            .spawn(move || relay(reader, &sender))
            .map_err(|source| spawn_error(program, source))?;
        Ok(started)
    }

    /// The child's process id.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Writes bytes to the child, as if typed.
    ///
    /// # Errors
    ///
    /// [`PtyError::Write`] when the terminal refuses them.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|source| PtyError::Write { source })
    }

    /// Reads until the child has produced nothing for `quiet` after it
    /// produced something, or `cap` elapses. Silence before the first byte
    /// is waiting, not quiet.
    ///
    /// # Errors
    ///
    /// [`PtyError::Timeout`] when `cap` elapses first, carrying the escaped
    /// bytes seen — an empty success is impossible; [`PtyError::Closed`]
    /// when the child closed its terminal having produced nothing.
    pub fn read_until_quiet(
        &mut self,
        quiet: Duration,
        cap: Duration,
    ) -> Result<Vec<u8>, PtyError> {
        let started = Instant::now();
        let mut received = Vec::new();
        loop {
            let remaining = cap.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(PtyError::Timeout {
                    received: escaped(&received),
                });
            }
            let quiet_wait = !received.is_empty() && quiet <= remaining;
            let wait = if quiet_wait { quiet } else { remaining };
            match self.chunks.recv_timeout(wait) {
                Ok(chunk) => received.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) if quiet_wait => return Ok(received),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(PtyError::Timeout {
                        received: escaped(&received),
                    });
                }
                Err(RecvTimeoutError::Disconnected) if received.is_empty() => {
                    return Err(PtyError::Closed {
                        received: String::new(),
                    });
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(received),
            }
        }
    }

    /// Resizes the terminal, which the kernel tells the child about.
    ///
    /// # Errors
    ///
    /// [`PtyError::Resize`] when the terminal refuses the size.
    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<(), PtyError> {
        self.master
            .resize(PtySize {
                rows,
                cols: columns,
                pixel_width: PIXEL_SIZE,
                pixel_height: PIXEL_SIZE,
            })
            .map_err(|source| PtyError::Resize {
                source: source.into(),
            })
    }

    /// Waits for the child to end and says how it did.
    ///
    /// # Errors
    ///
    /// [`PtyError::Wait`] when the child cannot be waited for.
    pub fn wait(mut self) -> Result<ExitStatus, PtyError> {
        let status = self
            .child
            .wait()
            .map_err(|source| PtyError::Wait { source })?;
        self.reaped = true;
        Ok(match status.signal() {
            Some(name) => ExitStatus::Signalled(name.to_owned()),
            None => ExitStatus::Exited(status.exit_code()),
        })
    }
}

impl Drop for PtyChild {
    /// Kills the child's whole process group and reaps the child, so a
    /// dropped harness leaves no process behind; the reader thread ends by
    /// itself when the terminal closes.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Ok(group) = i32::try_from(self.process_id) {
            let _killed = killpg(Pid::from_raw(group), Signal::SIGKILL);
        }
        let _reaped = self.child.wait();
    }
}

/// The reader thread's body: chunks from the terminal to the channel until
/// the terminal closes or nobody listens.
fn relay(mut reader: Box<dyn Read + Send>, sender: &mpsc::Sender<Vec<u8>>) {
    let mut buffer = vec![0; READ_LENGTH];
    loop {
        match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Ok(0) | Err(_) => return,
            Ok(count) => {
                let chunk = buffer.get(..count).unwrap_or_default().to_vec();
                if sender.send(chunk).is_err() {
                    return;
                }
            }
        }
    }
}
