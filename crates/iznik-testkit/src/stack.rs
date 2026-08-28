//! The in-process stack: a daemon under a temporary runtime directory, in
//! process or as a binary, torn down on drop.
//!
//! Every later integration test is written against this, and none of them
//! guesses a binary's path. `CARGO_BIN_EXE_iznik-server` exists only inside
//! the package that builds the binary, so a test elsewhere that reached for
//! one would find a stale artifact or none — which is why the default mode
//! runs the daemon's own `serve` on a thread this owns, and the binary mode is
//! for the server package's own tests, which have the path from Cargo.
//!
//! Whatever it started, it ends: the daemon, its panes and its runtime
//! directory go when the stack is dropped, on a clean end and on a panic
//! alike.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use core::fmt::{self, Display, Formatter};

use iznik_server::daemon::{self, DaemonOptions, PathsError, RuntimePaths};
use iznik_server::pty::spawn::Program;
use tokio::sync::watch;

/// How long a stack may take to answer before it is called stopped rather
/// than starting.
pub const STARTUP_CEILING: Duration = Duration::from_millis(500);

/// How long between looks at a socket that is about to appear.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The idle interval a stack's daemon runs under: long, because a stack goes
/// when it is dropped and not when it is quiet.
const PATIENT: Duration = Duration::from_hours(1);

/// Tells one stack's directory from another's in the same process.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Where the daemon runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonMode {
    /// On a thread this stack owns, through `daemon::serve` itself. The
    /// default, and what every test outside the server package uses.
    InProcess,
    /// As the binary at this path, run with `--foreground`. For the server
    /// package's own tests, which have the path from Cargo.
    Binary(PathBuf),
}

/// What a stack is started with.
#[derive(Clone, Debug)]
pub struct StackOptions {
    /// Where the daemon runs.
    pub daemon: DaemonMode,
    /// How long with nothing to hold before it would exit on its own.
    pub idle_shutdown: Duration,
    /// What its panes run. `sh`, because a login shell draws a prompt
    /// asynchronously and a test would wait for it.
    pub program: Program,
}

impl Default for StackOptions {
    fn default() -> StackOptions {
        StackOptions {
            daemon: DaemonMode::InProcess,
            idle_shutdown: PATIENT,
            program: Program::Command {
                path: "sh".into(),
                arguments: Vec::new(),
            },
        }
    }
}

/// Why a stack could not be started.
#[derive(Debug)]
pub enum StackError {
    /// The runtime directory could not be made.
    Paths(PathsError),
    /// The temporary directory could not be made or removed.
    Io {
        /// The directory.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The binary could not be started.
    Binary {
        /// The path that was asked for.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The daemon never answered.
    Silent {
        /// The socket it should have answered on.
        socket: PathBuf,
        /// How long it was given.
        waited: Duration,
    },
    /// A daemon run as a binary was asked for a program with arguments, and
    /// its command line has no way to say them.
    Unsayable {
        /// The program.
        path: PathBuf,
        /// The arguments that would have been dropped.
        arguments: Vec<String>,
    },
}

impl Display for StackError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            StackError::Paths(source) => write!(formatter, "{source}"),
            StackError::Io { path, source } => {
                write!(formatter, "the directory {}: {source}", path.display())
            }
            StackError::Binary { path, source } => {
                write!(formatter, "the binary {}: {source}", path.display())
            }
            StackError::Silent { socket, waited } => write!(
                formatter,
                "nothing answered on {} within {waited:?}",
                socket.display()
            ),
            StackError::Unsayable { path, arguments } => write!(
                formatter,
                "a daemon run as a binary cannot be told to run {} with {arguments:?}: \
                 `--program` takes a path and nothing after it, and dropping them \
                 would make this stack and an in-process one run different things",
                path.display()
            ),
        }
    }
}

impl core::error::Error for StackError {}

impl From<PathsError> for StackError {
    fn from(source: PathsError) -> StackError {
        StackError::Paths(source)
    }
}

/// The daemon a stack is holding.
#[derive(Debug)]
enum Running {
    /// A thread of this process, and the switch that stops it.
    Here {
        /// Flipped to ask the daemon to stop.
        asked: watch::Sender<bool>,
        /// The thread the daemon's runtime lives on.
        thread: Option<std::thread::JoinHandle<()>>,
    },
    /// A child process.
    Apart {
        /// The child.
        child: std::process::Child,
    },
}

/// A daemon under a runtime directory of its own.
#[derive(Debug)]
pub struct Stack {
    /// The temporary directory everything lives under.
    home: PathBuf,
    /// The paths the daemon is using.
    paths: RuntimePaths,
    /// What is running, and how to end it.
    running: Running,
}

impl Stack {
    /// Starts a daemon and waits for its socket to answer.
    ///
    /// # Errors
    ///
    /// [`StackError::Io`] when the temporary directory cannot be made,
    /// [`StackError::Binary`] naming the path when a binary will not start,
    /// and [`StackError::Silent`] when nothing answers within
    /// [`STARTUP_CEILING`].
    pub async fn start(options: StackOptions) -> Result<Stack, StackError> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let numbered = NEXT.fetch_add(1, Ordering::Relaxed);
        let home = base.join(format!("iznik-stack-{}-{numbered}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(|source| StackError::Io {
            path: home.clone(),
            source,
        })?;
        let paths = RuntimePaths::under(&home.join("iznik"))?;
        let running = begin(&options, &home, &paths)?;
        let stack = Stack {
            home,
            paths,
            running,
        };
        let started = Instant::now();
        while started.elapsed() < STARTUP_CEILING {
            if tokio::net::UnixStream::connect(&stack.paths.socket)
                .await
                .is_ok()
            {
                return Ok(stack);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Err(StackError::Silent {
            socket: stack.paths.socket.clone(),
            waited: STARTUP_CEILING,
        })
    }

    /// The socket a client connects to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.paths.socket
    }

    /// The runtime directory everything of this daemon's lives under.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.paths.directory
    }
}

/// Starts the daemon the options ask for.
///
/// # Errors
///
/// [`StackError::Binary`] naming the path when a binary will not start.
fn begin(options: &StackOptions, home: &Path, paths: &RuntimePaths) -> Result<Running, StackError> {
    match &options.daemon {
        DaemonMode::InProcess => {
            let (asked, shutdown) = watch::channel(false);
            let settings = DaemonOptions {
                idle_shutdown: options.idle_shutdown,
                program: options.program.clone(),
                ..DaemonOptions::default()
            };
            let held = paths.clone();
            // Its own runtime on its own thread: a daemon is a process in the
            // product, and a stack that ran it on the caller's runtime would
            // make every test's scheduling the daemon's too.
            let thread = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let _served = runtime.block_on(daemon::serve(&held, settings, shutdown));
            });
            Ok(Running::Here {
                asked,
                thread: Some(thread),
            })
        }
        DaemonMode::Binary(path) => {
            let mut running = std::process::Command::new(path);
            running
                .arg("--foreground")
                .arg("--idle-shutdown-seconds")
                .arg(options.idle_shutdown.as_secs().to_string());
            // Without this the child would run the product's default, which is
            // whoever's login shell is on the machine, and a stack that said
            // `sh` would have been measuring something else.
            //
            // `--program` takes a path and nothing after it. A caller who
            // wants arguments is asking for something this mode cannot do, and
            // silently running the program without them would make the two
            // modes disagree about what a pane is.
            if let Program::Command {
                path: named,
                arguments,
            } = &options.program
            {
                if !arguments.is_empty() {
                    return Err(StackError::Unsayable {
                        path: named.clone(),
                        arguments: arguments.clone(),
                    });
                }
                running.arg("--program").arg(named);
            }
            let child = running
                .env("XDG_RUNTIME_DIR", home)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|source| StackError::Binary {
                    path: path.clone(),
                    source,
                })?;
            Ok(Running::Apart { child })
        }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        match &mut self.running {
            Running::Here { asked, thread } => {
                let _told = asked.send(true);
                if let Some(thread) = thread.take() {
                    let _joined = thread.join();
                }
            }
            Running::Apart { child } => {
                let _told = child.kill();
                let _waited = child.wait();
            }
        }
        let _gone = std::fs::remove_dir_all(&self.home);
    }
}
