//! Spawning proven against the kernel: a login shell that initializes as one, a
//! child that is its own session leader on the pseudoterminal, the environment
//! a pane runs under, faithful exit statuses, and a drop that leaves nothing
//! behind. Every child is `sh` but the one login-shell case, and every read is a
//! `read_until_quiet` with a sub-second quiet interval.

use std::error::Error;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use iznik_server::pty::spawn::{
    ExitStatus, Program, PtyError, PtyProcess, Signal, SpawnOptions, spawn,
};

/// The quiet interval that means a child has finished answering.
const QUIET: Duration = Duration::from_millis(300);

/// The most a read waits for any output at all.
const CAP: Duration = Duration::from_secs(5);

/// The control byte a script brackets its output with. `printf` writes it, but
/// the shell's echo of the `\001` in the command text does not, so the read can
/// tell the command's output from its echo.
const OUTPUT_MARKER: u8 = 0x01;

/// Anything that went wrong setting a session up.
type Setup = Box<dyn Error + Send + Sync>;

/// A background reader over a pseudoterminal master, so a blocking descriptor
/// can be read to quiet without blocking the test.
struct Reader {
    /// Chunks the reader thread has seen.
    chunks: mpsc::Receiver<Vec<u8>>,
    /// The reader thread, which ends when the terminal closes.
    _thread: thread::JoinHandle<()>,
}

impl Reader {
    /// A reader draining `source` on its own thread.
    fn new(mut source: Box<dyn Read + Send>) -> Reader {
        let (sender, chunks) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut buffer = vec![0; 4096];
            while let Ok(count) = source.read(&mut buffer) {
                let chunk = buffer.get(..count).unwrap_or_default().to_vec();
                if count == 0 || sender.send(chunk).is_err() {
                    return;
                }
            }
        });
        Reader {
            chunks,
            _thread: thread,
        }
    }

    /// The bytes seen until none arrive for `QUIET`, or `CAP` elapses, as text.
    fn read_until_quiet(&self) -> String {
        let start = Instant::now();
        let mut seen = Vec::new();
        loop {
            let elapsed = start.elapsed();
            if elapsed >= CAP {
                break;
            }
            match self
                .chunks
                .recv_timeout(QUIET.min(CAP.saturating_sub(elapsed)))
            {
                Ok(chunk) => seen.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) if !seen.is_empty() => break,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        String::from_utf8_lossy(&seen).into_owned()
    }

    /// Reads until two `marker` bytes have arrived, or the cap elapses. A lull
    /// while a login shell is still starting, or right after it echoes the
    /// command, does not end the read early the way `read_until_quiet` would, so
    /// the command's own output — bracketed by `marker` bytes a shell echo does
    /// not carry — is always waited for.
    fn read_until_pair(&self, marker: u8) -> Vec<u8> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < CAP {
            let remaining = CAP.saturating_sub(start.elapsed());
            match self.chunks.recv_timeout(QUIET.min(remaining)) {
                Ok(chunk) => {
                    seen.extend_from_slice(&chunk);
                    // A third split segment means both bracketing markers have
                    // arrived — the output between them is complete.
                    if seen.split(|&byte| byte == marker).nth(2).is_some() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        seen
    }
}

/// A spawned shell the test talks to: the process, a reader, and a writer.
struct Session {
    /// The spawned process.
    process: PtyProcess,
    /// The reader over its output.
    reader: Reader,
    /// The writer to its input.
    writer: Box<dyn Write + Send>,
}

impl Session {
    /// Spawns `options` and wires a reader and writer to its master.
    ///
    /// # Errors
    ///
    /// When the spawn or the master's reader or writer cannot be had.
    fn start(options: &SpawnOptions) -> Result<Session, Setup> {
        let process = spawn(options)?;
        let reader = Reader::new(process.master().try_clone_reader()?);
        let writer = process.master().take_writer()?;
        Ok(Session {
            process,
            reader,
            writer,
        })
    }

    /// Writes a line to the child.
    ///
    /// # Errors
    ///
    /// When the write fails.
    fn send_line(&mut self, line: &str) -> Result<(), std::io::Error> {
        writeln!(self.writer, "{line}")?;
        self.writer.flush()
    }
}

/// Spawn options for `sh -c script`, at eighty by twenty-four, no directories.
fn sh(script: &str) -> SpawnOptions {
    SpawnOptions {
        program: Program::Command {
            path: PathBuf::from("sh"),
            arguments: vec!["-c".to_owned(), script.to_owned()],
        },
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
    }
}

/// Spawn options for a bare program with arguments, no directories.
fn program(path: &str, arguments: &[&str]) -> SpawnOptions {
    SpawnOptions {
        program: Program::Command {
            path: PathBuf::from(path),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        },
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
    }
}

/// The bytes between the last two `Z` markers a script printed — the last pair,
/// so an interactive shell's echo of the command that printed them, which
/// carries the same markers, does not confuse the read.
fn marked(text: &str) -> Option<&str> {
    let (before, _after) = text.rsplit_once('Z')?;
    let (_start, inside) = before.rsplit_once('Z')?;
    Some(inside)
}

/// A login shell initializes as one — `argv[0]` begins with `-`. This is the one
/// case that spawns it.
///
/// # Panics
///
/// When `$0` does not begin with `-`.
#[test]
fn pty_spawn_the_login_shell_initializes_as_one() {
    let options = SpawnOptions {
        program: Program::LoginShell,
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
    };
    let mut session = Session::start(&options).expect("the login shell starts");
    session
        .send_line("printf '\\001%s\\001\\n' \"$0\"")
        .expect("the command is written");
    let output = session.reader.read_until_pair(OUTPUT_MARKER);
    let name = output
        .split(|&byte| byte == OUTPUT_MARKER)
        .nth(1)
        .unwrap_or_else(|| panic!("no marked name in {output:?}"));
    let name = String::from_utf8_lossy(name);
    assert!(
        name.starts_with('-'),
        "a login shell's $0 begins with -: {name:?}"
    );
    let _sent = session.send_line("exit");
}

/// The child is its own session leader on the pseudoterminal's device.
///
/// # Panics
///
/// When the session id is not the child's, or the terminal is not a `pts`.
#[test]
fn pty_spawn_the_child_leads_its_own_session_on_the_terminal() {
    let session = Session::start(&sh("ps -o sid= -o tty= -p $$")).expect("sh starts");
    let output = session.reader.read_until_quiet();
    let mut fields = output.split_whitespace();
    let session_id: u32 = fields
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no session id in {output:?}"));
    let terminal = fields.next().unwrap_or_default();
    assert_eq!(
        session_id,
        session.process.process_id(),
        "the child leads its session"
    );
    assert!(
        terminal.contains("pts"),
        "the terminal is a pseudoterminal: {terminal:?}"
    );
}

/// With a terminfo directory the ghostty `TERM` and `TERMINFO` are set; without
/// it the fallback `TERM` is set and `TERMINFO` is absent. `COLORTERM` and
/// `TERM_PROGRAM` are always set.
///
/// # Panics
///
/// When any variable is not as the pane's environment specifies.
#[test]
fn pty_spawn_the_environment_is_the_panes() {
    let script = "printf 'Z%s|%s|%s|%sZ\\n' \"$TERM\" \"${TERMINFO-absent}\" \"$COLORTERM\" \"$TERM_PROGRAM\"";
    {
        let mut options = sh(script);
        options.terminfo_directory = Some(PathBuf::from("/usr/share/terminfo"));
        let session = Session::start(&options).expect("sh starts");
        let output = session.reader.read_until_quiet();
        let fields =
            marked(&output).unwrap_or_else(|| panic!("no marked environment in {output:?}"));
        assert_eq!(fields, "xterm-ghostty|/usr/share/terminfo|truecolor|iznik");
    }
    {
        let session = Session::start(&sh(script)).expect("sh starts");
        let output = session.reader.read_until_quiet();
        let fields =
            marked(&output).unwrap_or_else(|| panic!("no marked environment in {output:?}"));
        assert_eq!(fields, "xterm-256color|absent|truecolor|iznik");
    }
}

/// The child starts in the working directory when it exists; a missing one
/// fails, naming the path and spawning nothing.
///
/// # Panics
///
/// When the directory is not honored, or the failure is not `WorkingDirectory`.
#[test]
fn pty_spawn_the_working_directory_is_honored_or_named() {
    let mut options = sh("printf 'Z%sZ\\n' \"$(pwd)\"");
    options.working_directory = Some(PathBuf::from("/tmp"));
    let session = Session::start(&options).expect("sh starts");
    let output = session.reader.read_until_quiet();
    assert_eq!(marked(&output), Some("/tmp"));

    let mut missing = sh("true");
    missing.working_directory = Some(PathBuf::from("/no/such/directory"));
    let error = spawn(&missing).expect_err("a missing directory fails the spawn");
    match error {
        PtyError::WorkingDirectory { path, .. } => {
            assert_eq!(path, PathBuf::from("/no/such/directory"));
        }
        other => panic!("expected WorkingDirectory, got {other:?}"),
    }
}

/// A program that does not exist fails, naming the path and spawning nothing.
///
/// # Panics
///
/// When the failure is not `Spawn` naming the path.
#[test]
fn pty_spawn_a_missing_program_fails_naming_it() {
    let error =
        spawn(&program("/no/such/program", &[])).expect_err("a missing program fails the spawn");
    match error {
        PtyError::Spawn { program, .. } => assert!(program.contains("/no/such/program")),
        other => panic!("expected Spawn, got {other:?}"),
    }
}

/// A resize is seen by the child: after `resize(100, 40)`, `stty size` prints
/// `40 100`.
///
/// # Panics
///
/// When the child does not see the new size.
#[test]
fn pty_spawn_a_resize_is_seen_by_the_child() {
    let session = Session::start(&sh("sleep 0.4; printf 'Z%sZ\\n' \"$(stty size)\"")).expect("sh");
    session
        .process
        .resize(100, 40)
        .expect("the resize succeeds");
    let output = session.reader.read_until_quiet();
    assert_eq!(marked(&output), Some("40 100"));
}

/// Exit statuses tell the truth: a code is the code, and a signal death is the
/// signal, never a fake code.
///
/// # Panics
///
/// When any status is not as it should be.
#[test]
fn pty_spawn_exit_statuses_tell_the_truth() {
    let mut exited = Session::start(&sh("exit 3")).expect("sh starts");
    assert_eq!(
        exited.process.wait().expect("waited"),
        ExitStatus::Exited(3)
    );

    let mut killed = Session::start(&sh("exec sleep 10")).expect("sh starts");
    killed
        .process
        .signal(Signal::Kill)
        .expect("the kill is sent");
    assert_eq!(
        killed.process.wait().expect("waited"),
        ExitStatus::Signalled(Signal::Kill)
    );

    let mut hung_up = Session::start(&sh("exec sleep 10")).expect("sh starts");
    hung_up
        .process
        .signal(Signal::Hangup)
        .expect("the hangup is sent");
    assert_eq!(
        hung_up.process.wait().expect("waited"),
        ExitStatus::Signalled(Signal::Hangup)
    );
}

/// Dropping a process kills its child: after a `sleep 30` is dropped, no such
/// process exists.
///
/// # Panics
///
/// When the child outlives the value that owned it.
#[test]
fn pty_spawn_a_drop_kills_the_child() {
    let session = Session::start(&program("sleep", &["30"])).expect("sleep starts");
    let process_id = session.process.process_id();
    drop(session);
    thread::sleep(Duration::from_millis(200));
    let alive = std::path::Path::new(&format!("/proc/{process_id}")).exists();
    assert!(!alive, "the child was reaped: /proc/{process_id} is gone");
}
