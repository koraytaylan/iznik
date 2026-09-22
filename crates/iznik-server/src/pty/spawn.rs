//! Opening a pseudoterminal pair and spawning the program in its own session,
//! with the environment a pane runs under and faithful exit statuses.
//!
//! `portable-pty` owns the `fork`/`exec` and the `setsid`/`TIOCSCTTY` that make
//! the child a session leader with the pseudoterminal as its controlling
//! terminal, so this crate stays `#![forbid(unsafe_code)]`. The child is waited
//! for through `nix`, not `portable-pty`, so a signal death is reported as the
//! signal it was — a number, not a locale-dependent description.

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use nix::sys::signal::{self, Signal as NixSignal};
#[cfg(unix)]
use nix::sys::wait::{WaitStatus, waitpid};
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(windows)]
use portable_pty::ChildKiller;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
#[cfg(windows)]
use std::sync::{Arc as Shared, Mutex};

/// The `TERM` variable's name.
const TERM_VARIABLE: &str = "TERM";

/// `TERM` when a ghostty terminfo directory is given.
const TERM_GHOSTTY: &str = "xterm-ghostty";

/// `TERM` when none is: the widely present fallback.
const TERM_FALLBACK: &str = "xterm-256color";

/// The `TERMINFO` variable's name.
const TERMINFO_VARIABLE: &str = "TERMINFO";

/// The `COLORTERM` variable's name.
const COLORTERM_VARIABLE: &str = "COLORTERM";

/// `COLORTERM`, always: the surface renders twenty-four-bit color.
const COLORTERM_VALUE: &str = "truecolor";

/// The `TERM_PROGRAM` variable's name.
const TERM_PROGRAM_VARIABLE: &str = "TERM_PROGRAM";

/// `TERM_PROGRAM`, always: what a program asking who renders it is told.
const TERM_PROGRAM_VALUE: &str = "iznik";

/// What to spawn on the pseudoterminal.
#[derive(Clone, Debug)]
pub enum Program {
    /// The current user's login shell, from the password database, initialized
    /// as a login shell — the daemon's registry always asks for this.
    LoginShell,
    /// A named program with arguments — for tests, which cannot use a login
    /// shell whose prompt draws itself asynchronously.
    Command {
        /// The executable, resolved against `PATH`.
        path: PathBuf,
        /// The arguments after `argv[0]`.
        arguments: Vec<String>,
    },
}

/// How to open a pane's pseudoterminal and what to run on it.
#[derive(Clone, Debug)]
pub struct SpawnOptions {
    /// What to spawn.
    pub program: Program,
    /// The initial width in columns.
    pub columns: u16,
    /// The initial height in rows.
    pub rows: u16,
    /// The directory to start in; the child fails to spawn if it is missing.
    pub working_directory: Option<PathBuf>,
    /// The terminfo directory a ghostty `TERM` needs; without it the fallback
    /// `TERM` is used and `TERMINFO` is left unset.
    pub terminfo_directory: Option<PathBuf>,
}

/// A signal the server sends, and the cause of a signal death it reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// `SIGHUP`.
    Hangup,
    /// `SIGTERM`.
    Terminate,
    /// `SIGKILL`.
    Kill,
}

/// How a child ended: a code it chose, or the signal that ended it — never a
/// fake code standing in for a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// It exited with this code.
    Exited(i32),
    /// A signal ended it.
    Signalled(Signal),
}

/// Why a pseudoterminal operation failed.
#[derive(Debug)]
pub enum PtyError {
    /// The pseudoterminal pair could not be opened.
    Open {
        /// What went wrong.
        source: Box<dyn Error + Send + Sync>,
    },
    /// The program could not be spawned.
    Spawn {
        /// The program asked for.
        program: String,
        /// What went wrong.
        source: Box<dyn Error + Send + Sync>,
    },
    /// The working directory does not exist or cannot be reached.
    WorkingDirectory {
        /// The directory asked for.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The pseudoterminal could not be resized.
    Resize {
        /// What went wrong.
        source: Box<dyn Error + Send + Sync>,
    },
    /// A signal could not be sent to the child.
    Signal {
        /// What the operating system said.
        source: Box<dyn Error + Send + Sync>,
    },
    /// The child could not be waited for.
    Wait {
        /// What the operating system said.
        source: Box<dyn Error + Send + Sync>,
    },
}

impl core::fmt::Display for PtyError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PtyError::Open { source } => write!(formatter, "opening the pseudoterminal: {source}"),
            PtyError::Spawn { program, source } => {
                write!(formatter, "spawning `{program}`: {source}")
            }
            PtyError::WorkingDirectory { path, source } => {
                write!(
                    formatter,
                    "the working directory {}: {source}",
                    path.display()
                )
            }
            PtyError::Resize { source } => write!(formatter, "resizing: {source}"),
            PtyError::Signal { source } => write!(formatter, "signalling the child: {source}"),
            PtyError::Wait { source } => write!(formatter, "waiting for the child: {source}"),
        }
    }
}

impl Error for PtyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PtyError::WorkingDirectory { source, .. } => Some(source),
            PtyError::Open { source }
            | PtyError::Spawn { source, .. }
            | PtyError::Resize { source }
            | PtyError::Signal { source }
            | PtyError::Wait { source } => Some(source.as_ref()),
        }
    }
}

/// A spawned program on a pseudoterminal, in its own session. Dropping it kills
/// the shell group and the terminal foreground group. Jobs detached from both
/// groups are outside terminal group signaling.
pub struct PtyProcess {
    /// The master end, kept open for the pane's lifetime and used to resize.
    master: Box<dyn MasterPty + Send>,
    /// The child, owned so its handle lives; it is waited for through `nix`.
    #[cfg(unix)]
    _child: Box<dyn Child + Send + Sync>,
    /// The child on Windows, taken by the reaper that waits for it.
    #[cfg(windows)]
    child: Shared<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    /// A handle that can end the child without waiting for it.
    #[cfg(windows)]
    child_stop: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// The child's process id, which is also its process group.
    process_id: u32,
    /// Whether the child has been reaped, so the drop need not kill it.
    reaped: Arc<AtomicBool>,
}

impl core::fmt::Debug for PtyProcess {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PtyProcess")
            .field("process_id", &self.process_id)
            .field("reaped", &self.reaped)
            .finish_non_exhaustive()
    }
}

impl PtyProcess {
    /// The child's process id.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    /// The pseudoterminal's master end, from which the caller clones a reader
    /// and takes the writer. `pty-streams` wraps them in async streams; a test
    /// reads and writes them directly. The blocking-I/O policy keeps `Read` and
    /// `Write` out of this crate's source outside `streams`, so this lends the
    /// master rather than naming a reader or writer here.
    #[must_use]
    pub fn master(&self) -> &(dyn MasterPty + Send) {
        self.master.as_ref()
    }

    /// Resizes the pseudoterminal, which sends the child `SIGWINCH`.
    ///
    /// # Errors
    ///
    /// [`PtyError::Resize`] when the resize cannot be applied.
    pub fn resize(&self, columns: u16, rows: u16) -> Result<(), PtyError> {
        self.master
            .resize(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|source| PtyError::Resize {
                source: source.into(),
            })
    }

    /// Sends a signal to the child.
    ///
    /// # Errors
    ///
    /// [`PtyError::Signal`] when the signal cannot be sent.
    pub fn signal(&self, signal: Signal) -> Result<(), PtyError> {
        #[cfg(unix)]
        {
            signal::kill(self.pid(), nix_signal(signal)).map_err(|source| PtyError::Signal {
                source: Box::new(source),
            })
        }
        #[cfg(windows)]
        {
            let _signal = signal;
            let mut child_stop = match self.child_stop.lock() {
                Ok(child_stop) => child_stop,
                Err(poisoned) => poisoned.into_inner(),
            };
            child_stop.kill().map_err(|source| PtyError::Signal {
                source: source.into(),
            })
        }
    }

    /// Waits for the child to end and says how it did.
    ///
    /// # Errors
    ///
    /// [`PtyError::Wait`] when the child cannot be waited for.
    pub fn wait(&mut self) -> Result<ExitStatus, PtyError> {
        self.reaper().wait()
    }

    /// Give the reaper a PID and shared completion flag, without borrowing the
    /// terminal owner across the blocking wait. Callers schedule exactly one wait
    /// and keep this owner alive until it returns.
    pub(crate) fn reaper(&self) -> ProcessReaper {
        ProcessReaper {
            #[cfg(unix)]
            process: self.pid(),
            #[cfg(windows)]
            child: Shared::clone(&self.child),
            reaped: Arc::clone(&self.reaped),
        }
    }

    /// Hang up the current foreground group, leaving the shell session alive
    /// until forced cleanup if that job ignores the signal. At a shell prompt,
    /// the shell itself is the foreground group and receives the hangup.
    ///
    /// # Errors
    /// Returns `Signal` when the foreground group cannot be signaled.
    pub(crate) fn hangup_terminal(&self) -> Result<(), PtyError> {
        #[cfg(unix)]
        {
            let foreground = self.foreground_group().unwrap_or_else(|| self.pid());
            signal::killpg(foreground, NixSignal::SIGHUP).map_err(|source| PtyError::Signal {
                source: Box::new(source),
            })
        }
        #[cfg(windows)]
        {
            self.signal(Signal::Hangup)
        }
    }

    /// Best-effort forced cleanup of the owned shell and its ordinary foreground
    /// job. Query while the session leader still exists; killing it first can
    /// detach the controlling terminal and lose the foreground identity.
    pub(crate) fn kill_terminal_groups(&self) {
        if self.reaped.load(Ordering::Acquire) {
            return;
        }
        #[cfg(unix)]
        {
            if let Some(foreground) = self.foreground_group()
                && foreground != self.pid()
            {
                let _killed = signal::killpg(foreground, NixSignal::SIGKILL);
            }
            let _killed = signal::killpg(self.pid(), NixSignal::SIGKILL);
        }
        #[cfg(windows)]
        {
            let mut child_stop = match self.child_stop.lock() {
                Ok(child_stop) => child_stop,
                Err(poisoned) => poisoned.into_inner(),
            };
            let _killed = child_stop.kill();
        }
    }

    /// Read a positive foreground group from the owned terminal descriptor.
    #[cfg(unix)]
    fn foreground_group(&self) -> Option<Pid> {
        self.master
            .process_group_leader()
            .filter(|group| *group > 0)
            .map(Pid::from_raw)
    }

    /// The child's process id as a [`Pid`], saturating an impossible overflow.
    #[cfg(unix)]
    fn pid(&self) -> Pid {
        Pid::from_raw(i32::try_from(self.process_id).unwrap_or(i32::MAX))
    }
}

impl Drop for PtyProcess {
    /// Stop the foreground job before the shell, then reap the owned child.
    fn drop(&mut self) {
        if self.reaped.load(Ordering::Acquire) {
            return;
        }
        self.kill_terminal_groups();
        let _reaped = self.reaper().wait();
    }
}

/// One blocking child wait, independent of the mutex guarding terminal operations.
#[derive(Debug)]
pub(crate) struct ProcessReaper {
    /// Owned shell PID; its PTY owner remains alive for the duration of the wait.
    #[cfg(unix)]
    process: Pid,
    /// The child on Windows, shared with the owner so only one wait runs.
    #[cfg(windows)]
    child: Shared<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
    /// Publish completion before any owner can attempt subsequent cleanup.
    reaped: Arc<AtomicBool>,
}

impl ProcessReaper {
    /// Reap the child and publish its status without holding a terminal mutex.
    ///
    /// # Errors
    /// Returns `Wait` when the kernel refuses the wait or reports an unexpected status.
    pub(crate) fn wait(self) -> Result<ExitStatus, PtyError> {
        #[cfg(unix)]
        {
            let status = waitpid(self.process, None).map_err(|source| PtyError::Wait {
                source: Box::new(source),
            })?;
            self.reaped.store(true, Ordering::Release);
            match status {
                WaitStatus::Exited(_pid, code) => Ok(ExitStatus::Exited(code)),
                WaitStatus::Signaled(_pid, signal, _dumped) => {
                    Ok(ExitStatus::Signalled(signal_of(signal)))
                }
                other => Err(PtyError::Wait {
                    source: format!("unexpected wait status: {other:?}").into(),
                }),
            }
        }
        #[cfg(windows)]
        {
            let mut held = match self.child.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(mut child) = held.take() else {
                self.reaped.store(true, Ordering::Release);
                return Ok(ExitStatus::Exited(0));
            };
            let status = child.wait().map_err(|source| PtyError::Wait {
                source: source.into(),
            })?;
            self.reaped.store(true, Ordering::Release);
            if status.signal().is_some() {
                Ok(ExitStatus::Signalled(Signal::Terminate))
            } else {
                Ok(ExitStatus::Exited(
                    i32::try_from(status.exit_code()).unwrap_or(i32::MAX),
                ))
            }
        }
    }
}

/// Spawns a program on a fresh pseudoterminal in its own session.
///
/// # Errors
///
/// [`PtyError::WorkingDirectory`] when `working_directory` is given and does not
/// exist, [`PtyError::Open`] when the pseudoterminal cannot be opened, and
/// [`PtyError::Spawn`] when the program cannot be started — each spawning
/// nothing.
pub fn spawn(options: &SpawnOptions) -> Result<PtyProcess, PtyError> {
    if let Some(directory) = &options.working_directory {
        let metadata =
            std::fs::metadata(directory).map_err(|source| PtyError::WorkingDirectory {
                path: directory.clone(),
                source,
            })?;
        if !metadata.is_dir() {
            return Err(PtyError::WorkingDirectory {
                path: directory.clone(),
                source: io::Error::from(io::ErrorKind::NotADirectory),
            });
        }
    }
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: options.rows,
            cols: options.columns,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|source| PtyError::Open {
            source: source.into(),
        })?;
    let command = command_of(options);
    let program = program_name(&options.program);
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|source| PtyError::Spawn {
            program: program.clone(),
            source: source.into(),
        })?;
    // A child with no process id cannot be signalled or reaped by pid, and a
    // zero would make `killpg`/`kill` target the daemon's own group; refuse it.
    let process_id = child.process_id().ok_or_else(|| PtyError::Spawn {
        program,
        source: "the spawned child reported no process id".into(),
    })?;
    let reaped = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        Ok(PtyProcess {
            master: pair.master,
            _child: child,
            process_id,
            reaped,
        })
    }
    #[cfg(windows)]
    {
        let child_stop = child.clone_killer();
        Ok(PtyProcess {
            master: pair.master,
            child: Shared::new(Mutex::new(Some(child))),
            child_stop: Mutex::new(child_stop),
            process_id,
            reaped,
        })
    }
}

/// The `portable-pty` command for a spawn: a login shell, or a named program,
/// with the pane environment set and the working directory chosen.
fn command_of(options: &SpawnOptions) -> CommandBuilder {
    let mut command = match &options.program {
        Program::LoginShell => CommandBuilder::new_default_prog(),
        Program::Command { path, arguments } => {
            let mut command = CommandBuilder::new(path);
            command.args(arguments);
            command
        }
    };
    if let Some(directory) = &options.terminfo_directory {
        command.env(TERM_VARIABLE, TERM_GHOSTTY);
        command.env(TERMINFO_VARIABLE, directory);
    } else {
        command.env(TERM_VARIABLE, TERM_FALLBACK);
        // The daemon's own environment is the SSH session that started it, so
        // a `TERMINFO` in it names that host's terminfo tree — not the one the
        // pane's `TERM` was found in, and on a host iznik is running from
        // elsewhere often not there at all. The fallback `TERM` promises a
        // pane whose curses library searches the system default, which is what
        // removing the variable makes true.
        command.env_remove(TERMINFO_VARIABLE);
    }
    command.env(COLORTERM_VARIABLE, COLORTERM_VALUE);
    command.env(TERM_PROGRAM_VARIABLE, TERM_PROGRAM_VALUE);
    if let Some(directory) = &options.working_directory {
        command.cwd(directory);
    }
    command
}

/// The name to report a spawn failure against.
fn program_name(program: &Program) -> String {
    match program {
        Program::LoginShell => "the login shell".to_owned(),
        Program::Command { path, .. } => path.display().to_string(),
    }
}

/// The signal a reported death carries. `Signal` is closed to the three the
/// server sends, so an unexpected death — a Ctrl-C's `SIGINT`, a crash's
/// `SIGSEGV` — is surfaced as `Terminate`: a signal death still, never a code.
#[cfg(unix)]
fn signal_of(signal: NixSignal) -> Signal {
    match signal {
        NixSignal::SIGHUP => Signal::Hangup,
        NixSignal::SIGKILL => Signal::Kill,
        _ => Signal::Terminate,
    }
}

/// The `nix` signal for one the server sends.
#[cfg(unix)]
fn nix_signal(signal: Signal) -> NixSignal {
    match signal {
        Signal::Hangup => NixSignal::SIGHUP,
        Signal::Terminate => NixSignal::SIGTERM,
        Signal::Kill => NixSignal::SIGKILL,
    }
}
