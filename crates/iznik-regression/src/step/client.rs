//! The `client` step: the protocol client driving a session over the standard
//! streams of a command the scenario names.
//!
//! The command is usually `ssh host0 /iznik/bin/iznik-server --stdio`, which is
//! how a scenario on the engine attaches to the daemon on the host through
//! real SSH before a client engine exists. It speaks the protocol through
//! `TestClient`, so the client under test in a container and the client under
//! test in process are the same one.
//!
//! `expect_reassembly` is the point of it: the pieces a scenario captured
//! across a break are fed into one emulator and the server's own screen into
//! another, and a difference fails the step. That is what says a reconnect
//! lost nothing.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::ToClient;
use iznik_testkit::client::{Received, TestClient};
use iznik_testkit::vt::Vt;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// How long the driver waits for silence when nothing says otherwise.
const DEFAULT_QUIET_MILLISECONDS: u64 = 300;

/// The pane width a session is made at when the step does not say.
const DEFAULT_COLUMNS: u16 = 80;

/// Its height.
const DEFAULT_ROWS: u16 = 24;

/// The `[steps.client]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// The command whose standard streams speak `iznik/1`.
    command: String,
    /// How long to wait for silence before an await gives up on more.
    #[serde(default)]
    until_quiet_milliseconds: Option<u64>,
    /// What to do, in order.
    actions: Vec<Action>,
}

/// One thing a client step does.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    /// The handshake.
    Hello,
    /// Make a session, and with it a tab and a pane.
    CreateSession {
        /// What to call it.
        name: String,
        /// The pane's width.
        #[serde(default)]
        columns: Option<u16>,
        /// Its height.
        #[serde(default)]
        rows: Option<u16>,
    },
    /// Begin delivery of a pane's output from where it is now.
    Subscribe {
        /// The pane.
        pane: u64,
    },
    /// Begin delivery from a sequence a file holds.
    Resume {
        /// The pane.
        pane: u64,
        /// The file the sequence was written to.
        from_file: PathBuf,
    },
    /// Ask for a pane's screen as it is now.
    ScreenRequest {
        /// The pane.
        pane: u64,
    },
    /// Return credit for every byte as it arrives.
    AutoCredit {
        /// Whether to.
        on: bool,
    },
    /// Send keystrokes.
    Input {
        /// The pane.
        pane: u64,
        /// What to type.
        text: String,
    },
    /// Read until a pane's bytes hold something.
    AwaitBytes {
        /// The pane.
        pane: u64,
        /// What they must hold.
        contains: String,
    },
    /// Write everything received on a pane's channel to a file.
    CaptureTo {
        /// The pane.
        pane: u64,
        /// Where to write it.
        path: PathBuf,
    },
    /// Write the last screen received for a pane to a file.
    ScreenTo {
        /// The pane.
        pane: u64,
        /// Where to write it.
        path: PathBuf,
    },
    /// Write where this client would resume from to a file.
    ResumePointTo {
        /// The pane.
        pane: u64,
        /// Where to write it.
        path: PathBuf,
    },
    /// Feed the pieces into one emulator and the server's own screen into
    /// another, and fail on a difference.
    ExpectReassembly {
        /// The pane.
        pane: u64,
        /// The files, in the order they are fed.
        pieces: Vec<PathBuf>,
    },
}

/// A child's standard streams as one duplex thing.
#[derive(Debug)]
struct Pipes {
    /// What goes to the command.
    input: ChildStdin,
    /// What comes back.
    output: ChildStdout,
}

impl AsyncRead for Pipes {
    fn poll_read(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        core::pin::Pin::new(&mut self.get_mut().output).poll_read(context, buffer)
    }
}

impl AsyncWrite for Pipes {
    fn poll_write(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
        bytes: &[u8],
    ) -> core::task::Poll<std::io::Result<usize>> {
        core::pin::Pin::new(&mut self.get_mut().input).poll_write(context, bytes)
    }

    fn poll_flush(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        core::pin::Pin::new(&mut self.get_mut().input).poll_flush(context)
    }

    fn poll_shutdown(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        core::pin::Pin::new(&mut self.get_mut().input).poll_shutdown(context)
    }
}

/// A session in progress: the client, what it has been told, and the size to
/// reproduce a screen at.
struct Session {
    /// The client under test.
    client: TestClient<Pipes>,
    /// The last screen each pane was sent.
    screens: std::collections::BTreeMap<PaneId, Vec<u8>>,
    /// The size panes are made and reproduced at.
    columns: u16,
    /// Their height.
    rows: u16,
    /// How long an await waits for silence.
    quiet: Duration,
}

impl Session {
    /// Reads one thing and records what it says.
    ///
    /// # Errors
    ///
    /// When nothing arrives within the quiet interval, which is how a reader
    /// knows there is no more.
    async fn take(&mut self) -> Result<(), ()> {
        let Ok(received) = self.client.next(self.quiet).await else {
            return Err(());
        };
        if let Received::Control(ToClient::Screen { pane, bytes, .. }) = received {
            let _held = self.screens.insert(pane, bytes);
        }
        Ok(())
    }

    /// Reads what is waiting until nothing more comes or the deadline passes.
    async fn settle(&mut self, deadline: Instant) {
        while Instant::now() < deadline {
            if self.take().await.is_err() {
                return;
            }
        }
    }

    /// Reads until a pane's bytes hold `wanted`.
    ///
    /// # Errors
    ///
    /// Words for a person when they never do.
    async fn await_bytes(
        &mut self,
        pane: PaneId,
        wanted: &str,
        deadline: Instant,
    ) -> Result<(), String> {
        while Instant::now() < deadline {
            if holds(self.client.bytes_of(pane), wanted.as_bytes()) {
                return Ok(());
            }
            if self.take().await.is_err() {
                break;
            }
        }
        if holds(self.client.bytes_of(pane), wanted.as_bytes()) {
            return Ok(());
        }
        Err(format!("pane {} never said {wanted:?}", pane.0))
    }

    /// Feeds the pieces into one emulator and the pane's own screen into
    /// another, and says whether they show the same thing.
    ///
    /// # Errors
    ///
    /// Words for a person when a piece cannot be read, the emulator refuses,
    /// or the two differ.
    async fn expect_reassembly(
        &mut self,
        pane: PaneId,
        pieces: &[PathBuf],
        deadline: Instant,
    ) -> Result<(), String> {
        let mut reassembled =
            Vt::new(self.columns, self.rows).map_err(|error| error.to_string())?;
        for piece in pieces {
            let bytes =
                std::fs::read(piece).map_err(|error| format!("{}: {error}", piece.display()))?;
            reassembled.feed(&bytes);
        }
        // The server's own truth, asked for now: what the pane looks like to
        // it, against what the pieces say it should.
        self.client
            .screen_request(pane)
            .await
            .map_err(|error| error.to_string())?;
        let _held = self.screens.remove(&pane);
        while Instant::now() < deadline && !self.screens.contains_key(&pane) {
            if self.take().await.is_err() {
                break;
            }
        }
        let truth = self
            .screens
            .get(&pane)
            .ok_or_else(|| format!("pane {} sent no screen to compare with", pane.0))?;
        let mut mirrored = Vt::new(self.columns, self.rows).map_err(|error| error.to_string())?;
        mirrored.feed(truth);
        let shown = reassembled.snapshot().map_err(|error| error.to_string())?;
        let expected = mirrored.snapshot().map_err(|error| error.to_string())?;
        if shown == expected {
            return Ok(());
        }
        Err(format!(
            "the pieces reassemble to a different screen than the pane's own:\n{shown}\n---\n{expected}"
        ))
    }
}

/// Whether a byte string holds another.
fn holds(held: &[u8], wanted: &[u8]) -> bool {
    !wanted.is_empty() && held.windows(wanted.len()).any(|piece| piece == wanted)
}

/// Does one action.
///
/// # Errors
///
/// Words for a person when the action fails; a step that fails is a non-zero
/// exit with the reason, not a harness error.
async fn act(session: &mut Session, action: &Action, deadline: Instant) -> Result<(), String> {
    let refused = |error: iznik_testkit::client::ClientError| error.to_string();
    match action {
        Action::Hello => {
            let _greeting = session
                .client
                .hello(Capabilities::from_bits(0))
                .await
                .map_err(refused)?;
        }
        Action::CreateSession {
            name,
            columns,
            rows,
        } => {
            session.columns = columns.unwrap_or(session.columns);
            session.rows = rows.unwrap_or(session.rows);
            let outcome = session
                .client
                .command(SessionCommand::CreateSession {
                    name: name.clone(),
                    columns: session.columns,
                    rows: session.rows,
                    working_directory: None,
                })
                .await
                .map_err(refused)?;
            if let CommandOutcome::Rejected { code, message } = outcome {
                return Err(format!("the session was refused ({code:?}): {message}"));
            }
        }
        Action::Subscribe { pane } => {
            session
                .client
                .subscribe(PaneId(*pane))
                .await
                .map_err(refused)?;
        }
        Action::Resume { pane, from_file } => {
            let said = std::fs::read_to_string(from_file)
                .map_err(|error| format!("{}: {error}", from_file.display()))?;
            let from: u64 = said
                .trim()
                .parse()
                .map_err(|_unparsed| format!("{}: not a sequence", from_file.display()))?;
            session
                .client
                .resume(PaneId(*pane), Sequence(from))
                .await
                .map_err(refused)?;
        }
        Action::ScreenRequest { pane } => {
            session
                .client
                .screen_request(PaneId(*pane))
                .await
                .map_err(refused)?;
        }
        Action::AutoCredit { on } => session.client.auto_credit(*on),
        Action::Input { pane, text } => {
            session
                .client
                .input(PaneId(*pane), text.clone().into_bytes())
                .await
                .map_err(refused)?;
        }
        Action::AwaitBytes { pane, contains } => {
            session
                .await_bytes(PaneId(*pane), contains, deadline)
                .await?;
        }
        Action::CaptureTo { pane, path } => {
            std::fs::write(path, session.client.bytes_of(PaneId(*pane)))
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Action::ScreenTo { pane, path } => {
            let held = session
                .screens
                .get(&PaneId(*pane))
                .ok_or_else(|| format!("pane {pane} has sent no screen"))?;
            std::fs::write(path, held).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Action::ResumePointTo { pane, path } => {
            let at = session.client.resume_point(PaneId(*pane));
            std::fs::write(path, at.0.to_string())
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Action::ExpectReassembly { pane, pieces } => {
            session
                .expect_reassembly(PaneId(*pane), pieces, deadline)
                .await?;
        }
    }
    session.settle(deadline).await;
    Ok(())
}

/// Runs every action against the command's standard streams.
///
/// # Errors
///
/// Words for a person when the command will not start or an action fails.
async fn drive(body: &Body, deadline: Instant) -> Result<String, String> {
    let mut child: Child = Command::new("sh")
        .arg("-c")
        .arg(&body.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{}: {error}", body.command))?;
    let input = child
        .stdin
        .take()
        .ok_or("the command has no standard input")?;
    let output = child
        .stdout
        .take()
        .ok_or("the command has no standard output")?;
    let quiet = Duration::from_millis(
        body.until_quiet_milliseconds
            .unwrap_or(DEFAULT_QUIET_MILLISECONDS),
    );
    let mut session = Session {
        client: TestClient::over(Pipes { input, output }),
        screens: std::collections::BTreeMap::new(),
        columns: DEFAULT_COLUMNS,
        rows: DEFAULT_ROWS,
        quiet,
    };
    let mut done = 0_usize;
    for action in &body.actions {
        act(&mut session, action, deadline).await?;
        done = done.saturating_add(1);
    }
    drop(session);
    let _told = child.start_kill();
    let _waited = child.wait().await;
    Ok(format!("{done} actions"))
}

/// The `client` step. Its body is the `[steps.client]` table.
///
/// # Errors
///
/// [`StepError::Malformed`] when the body is not the table this expects, and
/// [`StepError::Input`] when a runtime cannot be built.
pub fn execute(
    _context: &Context,
    body: &toml::Value,
    timeout: Duration,
) -> Result<Outcome, StepError> {
    let asked: Body =
        body.clone()
            .try_into()
            .map_err(|error: toml::de::Error| StepError::Malformed {
                detail: format!("a `client` step: {error}"),
            })?;
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;
    let started = Instant::now();
    // A timeout so large it does not fit an instant is the runner's business,
    // not this step's; it is capped rather than refused.
    let deadline = started.checked_add(timeout).unwrap_or(started);
    let driven = runtime.block_on(drive(&asked, deadline));
    let duration = started.elapsed();
    Ok(match driven {
        Ok(summary) => Outcome {
            exit: Some(0),
            timed_out: false,
            duration,
            stdout: summary,
            stderr: String::new(),
        },
        Err(reason) => Outcome {
            exit: Some(1),
            timed_out: false,
            duration,
            stdout: String::new(),
            stderr: reason,
        },
    })
}
