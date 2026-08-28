//! The `channel` step: one channel to a host over real SSH, and what it does
//! when the link stops answering.
//!
//! The client step one file over drives a `TestClient` through the standard
//! streams of a command a scenario names; this drives a `RemoteChannel`
//! through the transport a scenario names, which is the thing the product
//! actually opens. What is proven here and nowhere else is the liveness: a
//! link cut in the middle is a `Dead` inside the deadline the scenario set,
//! not a TCP timeout minutes later.
//!
//! Its vocabulary is the client step's, narrowed to what a channel scenario
//! needs — the handshake is the opening, so there is no `hello` action, and
//! everything about reassembly belongs to the client step that has an oracle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iznik_client::transport::channel::{ChannelError, ChannelOptions, RemoteChannel};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, Transport};
use iznik_protocol::command::{
    CommandOutcome, SessionCommand, decode_command_outcome, encode_session_command,
};
use iznik_protocol::identity::{CommandId, PaneId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, ToClient, ToServer, decode_to_client, encode_to_server,
};
use serde::Deserialize;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// The pane width a session is made at when the step does not say.
const DEFAULT_COLUMNS: u16 = 80;

/// Its height.
const DEFAULT_ROWS: u16 = 24;

/// How long one wait for a particular thing may take when the step does not
/// say otherwise.
const DEFAULT_PATIENCE: Duration = Duration::from_secs(20);

/// The `[steps.channel]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// The host alias, as `~/.ssh/config` names it.
    alias: String,
    /// The server to run there.
    server: PathBuf,
    /// How often to ask whether it is still there.
    #[serde(default)]
    ping_interval_milliseconds: Option<u64>,
    /// How long silence may last before the link is dead.
    #[serde(default)]
    pong_deadline_milliseconds: Option<u64>,
    /// When set, the step succeeds only if the channel reports itself dead
    /// inside this many milliseconds.
    #[serde(default)]
    expect_dead_within_milliseconds: Option<u64>,
    /// What to do, in order.
    #[serde(default)]
    actions: Vec<Action>,
}

/// One thing a channel step does.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    /// Make a session, and with it a tab and a pane.
    CreateSession {
        /// What to call it.
        name: String,
    },
    /// Begin delivery of a pane's output.
    Subscribe {
        /// The pane.
        pane: u64,
    },
    /// Type into a pane.
    Input {
        /// The pane.
        pane: u64,
        /// What to type.
        text: String,
    },
    /// Wait until a pane's bytes contain something.
    AwaitBytes {
        /// The pane.
        pane: u64,
        /// What must appear.
        contains: String,
    },
    /// Stop the daemon on the host from answering, without closing anything.
    ///
    /// This is how a scenario makes a link go silent inside one step, which is
    /// where it has to happen: each step is its own process, so a channel
    /// opened in one is gone by the next, and a network cut between them would
    /// be met by the *opening* of a second channel rather than by the silence
    /// of the first. A stopped daemon leaves the relay running and the socket
    /// open and answers nothing, which is exactly the link this notices.
    PauseServer,
    /// Let it answer again, so the fixture tears down as it should.
    ResumeServer,
}

/// The shell that finds the daemon's lock wherever the host put it and sends
/// it a signal.
///
/// The same two places `RuntimePaths::resolve` looks, in the same order.
fn signal_the_daemon(signal: &str) -> String {
    format!(
        "if [ -n \"$XDG_RUNTIME_DIR\" ]; then lock=\"$XDG_RUNTIME_DIR/iznik/server.lock\"; \
         else lock=\"${{TMPDIR:-/tmp}}/iznik-$(id -u)/server.lock\"; fi; \
         kill -{signal} \"$(cat \"$lock\")\""
    )
}

/// What a channel has been told about a pane, which is what an `await_bytes`
/// looks in.
#[derive(Default)]
struct Heard {
    /// Which pane each channel carries.
    panes: std::collections::BTreeMap<u8, PaneId>,
    /// The bytes each pane has sent.
    bytes: std::collections::BTreeMap<PaneId, Vec<u8>>,
}

/// The options a step asks for.
fn options(body: &Body) -> ChannelOptions {
    let mut options = ChannelOptions::default();
    if let Some(milliseconds) = body.ping_interval_milliseconds {
        options.ping_interval = Duration::from_millis(milliseconds);
    }
    if let Some(milliseconds) = body.pong_deadline_milliseconds {
        options.pong_deadline = Duration::from_millis(milliseconds);
    }
    options
}

/// Reads one frame and files what it says under the pane it belongs to.
///
/// # Errors
///
/// Whatever the channel reports.
async fn take_one(
    channel: &mut RemoteChannel,
    heard: &mut Heard,
    deadline: Instant,
) -> Result<Option<ToClient>, ChannelError> {
    let frame = channel.next(deadline).await?;
    if frame.channel != CHANNEL_CONTROL {
        if let Some(pane) = heard.panes.get(&frame.channel).copied() {
            heard.bytes.entry(pane).or_default().extend(frame.payload);
        }
        return Ok(None);
    }
    let message = decode_to_client(&frame.payload).map_err(ChannelError::Message)?;
    if let ToClient::PaneChannel {
        channel: number,
        pane,
        ..
    } = &message
    {
        heard.panes.insert(*number, *pane);
    }
    Ok(Some(message))
}

/// Waits until `wanted` says a message is the one, filing everything else.
///
/// # Errors
///
/// The step's own words when the deadline passes with nothing matching.
async fn await_message(
    channel: &mut RemoteChannel,
    heard: &mut Heard,
    deadline: Instant,
    wanted: impl Fn(&ToClient) -> bool,
) -> Result<ToClient, String> {
    loop {
        match take_one(channel, heard, deadline).await {
            Ok(Some(message)) if wanted(&message) => return Ok(message),
            Ok(_other) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Does one action.
///
/// # Errors
///
/// The step's own words when the action does not happen.
async fn act(
    channel: &mut RemoteChannel,
    transport: &Transport,
    heard: &mut Heard,
    action: &Action,
    deadline: Instant,
) -> Result<(), String> {
    match action {
        Action::CreateSession { name } => {
            let payload = encode_session_command(&SessionCommand::CreateSession {
                name: name.clone(),
                columns: DEFAULT_COLUMNS,
                rows: DEFAULT_ROWS,
                working_directory: None,
            })
            .map_err(|error| error.to_string())?;
            send(
                channel,
                &ToServer::Command {
                    command_id: CommandId(0),
                    payload,
                },
            )
            .await?;
            let answered = await_message(channel, heard, deadline, |message| {
                matches!(message, ToClient::CommandResult { .. })
            })
            .await?;
            let ToClient::CommandResult { payload: said, .. } = answered else {
                return Err("the answer was not a command result".to_owned());
            };
            let outcome = decode_command_outcome(&said).map_err(|error| error.to_string())?;
            if let CommandOutcome::Rejected { code, message } = outcome {
                return Err(format!("the session was refused ({code:?}): {message}"));
            }
            Ok(())
        }
        Action::Subscribe { pane } => {
            send(
                channel,
                &ToServer::Subscribe {
                    pane: PaneId(*pane),
                },
            )
            .await?;
            let _announced = await_message(channel, heard, deadline, |message| {
                matches!(message, ToClient::PaneChannel { .. })
            })
            .await?;
            Ok(())
        }
        Action::Input { pane, text } => {
            send(
                channel,
                &ToServer::Input {
                    pane: PaneId(*pane),
                    bytes: text.clone().into_bytes(),
                },
            )
            .await
        }
        Action::PauseServer | Action::ResumeServer => {
            let signal = if matches!(action, Action::PauseServer) {
                "STOP"
            } else {
                "CONT"
            };
            let Transport::Ssh(ssh) = transport else {
                return Err("pausing a server needs an alias ssh reaches".to_owned());
            };
            let spawned = ssh
                .spawn(&[signal_the_daemon(signal)])
                .map_err(|error| error.to_string())?;
            let said = spawned
                .child
                .wait_with_output()
                .await
                .map_err(|source| format!("the signal could not be sent: {source}"))?;
            if said.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "the daemon could not be sent {signal}: {}",
                    String::from_utf8_lossy(&said.stderr).trim()
                ))
            }
        }
        Action::AwaitBytes { pane, contains } => {
            let pane = PaneId(*pane);
            let wanted = contains.as_bytes();
            loop {
                if heard
                    .bytes
                    .get(&pane)
                    .is_some_and(|held| held.windows(wanted.len()).any(|piece| piece == wanted))
                {
                    return Ok(());
                }
                take_one(channel, heard, deadline)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
    }
}

/// Sends one control message.
///
/// # Errors
///
/// The step's own words when it cannot be coded or sent.
async fn send(channel: &mut RemoteChannel, message: &ToServer) -> Result<(), String> {
    let payload = encode_to_server(message).map_err(|error| error.to_string())?;
    channel
        .send(CHANNEL_CONTROL, &payload)
        .await
        .map_err(|error| error.to_string())
}

/// Runs the step and says what it established.
///
/// # Errors
///
/// The step's own words when what happened is not what was asked for.
async fn drive(body: &Body, deadline: Instant) -> Result<String, String> {
    let paths = ClientRuntimePaths::resolve().map_err(|error| error.to_string())?;
    let transport = Transport::for_alias(&body.alias, &paths, SshOptions::default());
    let mut channel = RemoteChannel::open(&transport, Some(&body.server), options(body))
        .await
        .map_err(|error| error.to_string())?;
    let greeting = channel.greeting().clone();
    let mut heard = Heard::default();
    let mut done = 0_usize;
    for action in &body.actions {
        act(&mut channel, &transport, &mut heard, action, deadline).await?;
        done = done.saturating_add(1);
    }
    let Some(milliseconds) = body.expect_dead_within_milliseconds else {
        return Ok(format!(
            "iznik/{} on {}, {done} actions",
            greeting.protocol_version, body.alias
        ));
    };
    // The link is expected to have stopped answering. What is asserted is not
    // that it is gone — a scenario cut it — but that the channel says so
    // inside its own deadline rather than after a transport timeout.
    let bound = Duration::from_millis(milliseconds);
    let started = Instant::now();
    let waiting = started.checked_add(bound).unwrap_or(started);
    // Whatever was already in flight when the server stopped is still coming —
    // a subscription's screen, the bytes after it — and delivering it is the
    // channel doing its job. What is asserted is what happens when there is
    // nothing left: silence, noticed, inside the bound.
    loop {
        match channel.next(waiting).await {
            Ok(_carried) => {}
            Err(ChannelError::Dead { host, silent_for }) => {
                return Ok(format!(
                    "{host} was dead after {silent_for:?}, said in {:?}",
                    started.elapsed()
                ));
            }
            other => {
                return Err(format!(
                    "a silent link was not called dead within {bound:?}: {other:?}"
                ));
            }
        }
    }
}

/// The `channel` step. Its body is the `[steps.channel]` table.
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
                detail: format!("a `channel` step: {error}"),
            })?;
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;
    let started = Instant::now();
    let patience = timeout.min(DEFAULT_PATIENCE.max(timeout));
    let deadline = started.checked_add(patience).unwrap_or(started);
    let driven = runtime.block_on(async {
        tokio::time::timeout(timeout, drive(&asked, deadline))
            .await
            .unwrap_or_else(|_elapsed| Err(format!("the channel step ran past {timeout:?}")))
    });
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
            timed_out: duration >= timeout,
            duration,
            stdout: String::new(),
            stderr: reason,
        },
    })
}
