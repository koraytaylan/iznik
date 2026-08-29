//! `iznik doctor <host>`: one JSON artifact that says which of five layers is
//! wrong, with secrets redacted by construction.
//!
//! Every section is present whether or not it could be filled: a layer that
//! failed is visible by having an error where its answer belongs, and the
//! layers below one that failed say they were skipped rather than pretending
//! to have been asked. So the shape of the document is the diagnosis.
//!
//! Redacted by construction and not by scrubbing: what could carry a secret
//! is never collected. Of everything `ssh -G` will say, this reads the
//! handful of settings below and nothing else — not an identity file, not an
//! agent socket, not a proxy command, each of which can carry key material or
//! a token or run a program that prints one. Nothing reads the environment.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use iznik_client::bootstrap::launch::BootstrapOptions;
use iznik_client::bootstrap::probe::{HostProbe, RunsRemotely, probe};
use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::HostState;
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX, Transport};
use iznik_protocol::message::PROTOCOL_VERSION;

use crate::benchmark::{measured, shaped};
use crate::output::{Value, line, object, refusal, text};
use crate::{CLIENT_LAYER, TRANSPORT_LAYER, USAGE_EXIT_CODE, one_host, runtime};

/// What this subcommand takes.
const USAGE: &str = "usage: iznik doctor <host>";

/// How many settings this reads of everything `ssh -G` will say.
const SETTINGS_READ: usize = 8;

/// The settings this reads of everything `ssh -G` will say.
///
/// Chosen for what they diagnose and for what they cannot carry: a host, a
/// port, a user and a handful of numbers. Anything naming a file, a socket or
/// a program is not here, and is never read — which is what "redacted by
/// construction" means, as against collecting a secret and then trying to
/// remove it.
const SSH_SETTINGS: [&str; SETTINGS_READ] = [
    "hostname",
    "port",
    "user",
    "batchmode",
    "compression",
    "connecttimeout",
    "controlmaster",
    "serveraliveinterval",
];

/// How long `ssh -G` is given before it is taken to be stuck.
///
/// It expands a configuration and does not connect — but a configuration may
/// tell it to run a program, so it is bounded like anything else that runs.
const SSH_DEADLINE: Duration = Duration::from_secs(10);

/// How often a bounded wait looks.
const LOOK: Duration = Duration::from_millis(20);

/// The most this waits for a host to answer.
///
/// A ceiling and not a bootstrap's patience: what this collects is what
/// happened, and a host still installing a server after a minute has said
/// something about itself worth writing down. The wait ends sooner than this
/// whenever the host either answers or fails once — see below.
const REACH_DEADLINE: Duration = Duration::from_mins(1);

/// How many keystrokes the round trip is measured over here.
///
/// Fewer than the benchmark's hundred: this is one section of a document
/// somebody is reading because something is wrong, not a measurement.
const KEYSTROKES: usize = 20;

/// How many lines of the daemon's log are carried.
const LOG_LINES: usize = 40;

/// What the daemon calls its log, in its runtime directory.
const LOG_NAME: &str = "server.log";

/// Where a daemon keeps its runtime directory on a host, which is the rule
/// the bootstrap uses to put it there.
const LOG_SCRIPT: &str = "if [ -n \"${XDG_RUNTIME_DIR:-}\" ]\nthen runtime=\"$XDG_RUNTIME_DIR/iznik\"\nelse runtime=\"${TMPDIR:-/tmp}/iznik-$(id -u)\"; fi";

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(alias) = one_host(arguments) else {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, USAGE);
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    let _printed = line(&mut std::io::stdout(), &bundle(&alias));
    // The bundle is the answer, whatever it says: a host that could not be
    // reached is what somebody ran this to find out.
    ExitCode::SUCCESS
}

/// Everything this collects, as one object.
fn bundle(alias: &str) -> Value {
    let local = alias.starts_with(LOCAL_PREFIX);
    let paths = ClientRuntimePaths::resolve();
    let ssh = if local {
        skipped("a socket on this machine is reached without ssh")
    } else {
        configured(alias)
    };
    let probed = if local {
        skipped("a socket is not a host to run a probe on")
    } else {
        asked(alias)
    };
    let reached = held(alias);
    object(vec![
        ("host", text(alias)),
        (
            "client",
            object(vec![
                ("version", text(env!("CARGO_PKG_VERSION"))),
                (
                    "protocol_version",
                    Value::Whole(u64::from(PROTOCOL_VERSION)),
                ),
                (
                    "runtime_directory",
                    paths.as_ref().map_or_else(
                        |source| failed(CLIENT_LAYER, &source.to_string()),
                        |held| text(&held.directory.display().to_string()),
                    ),
                ),
            ]),
        ),
        ("ssh", ssh),
        ("probe", probed),
        ("server", reached.server),
        ("host_state", reached.state),
        ("round_trip", reached.round_trip),
    ])
}

/// A section that could not be filled, and why.
fn failed(layer: &str, detail: &str) -> Value {
    object(vec![("error", text(detail)), ("layer", text(layer))])
}

/// A section that was not asked for, and why.
fn skipped(why: &str) -> Value {
    object(vec![("skipped", text(why))])
}

/// What `ssh` says it would do for this host, of the settings above.
fn configured(alias: &str) -> Value {
    let mut asking = Command::new("ssh");
    asking
        .arg("-G")
        .arg(alias)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match bounded(asking, SSH_DEADLINE) {
        Ok(said) => kept(&said),
        Err(detail) => failed(TRANSPORT_LAYER, &detail),
    }
}

/// The settings this keeps of everything `ssh` says it would do.
///
/// The whole of the redaction, in one place and over text: what is kept is
/// the handful named in `SSH_SETTINGS`, and everything else — an identity
/// file, an agent socket, a command to run — is never looked at, let alone
/// written down. Separate from the running of `ssh` so that what it keeps
/// can be asked of it directly, which is the only way to be sure of what it
/// leaves out.
#[must_use]
pub fn kept(said: &str) -> Value {
    let mut fields: Vec<(&str, Value)> = Vec::new();
    for named in SSH_SETTINGS {
        if let Some(value) = said
            .lines()
            .filter_map(|line| line.trim().split_once(' '))
            .find(|(key, _value)| key.eq_ignore_ascii_case(named))
            .map(|(_key, value)| value.trim())
        {
            fields.push((named, text(value)));
        }
    }
    // What was asked for, so that a reader knows the absence of a setting is
    // this program's choice and not the configuration's silence.
    fields.push((
        "read",
        Value::List(SSH_SETTINGS.iter().map(|named| text(named)).collect()),
    ));
    object(fields)
}

/// Runs a command with a deadline and gives back what it printed.
///
/// # Errors
///
/// What went wrong, in words, including the deadline passing.
fn bounded(mut command: Command, deadline: Duration) -> Result<String, String> {
    let mut child = command.spawn().map_err(|source| source.to_string())?;
    let expires = Instant::now()
        .checked_add(deadline)
        .ok_or_else(|| "no clock".to_owned())?;
    while Instant::now() < expires {
        match child.try_wait() {
            Ok(Some(_status)) => {
                let done = child
                    .wait_with_output()
                    .map_err(|source| source.to_string())?;
                return Ok(String::from_utf8_lossy(&done.stdout).into_owned());
            }
            Ok(None) => std::thread::sleep(LOOK),
            Err(source) => return Err(source.to_string()),
        }
    }
    let _killed = child.kill();
    Err(format!("it was still running after {deadline:?}"))
}

/// What a probe of the host says.
fn asked(alias: &str) -> Value {
    let Ok(held) = runtime() else {
        return failed(CLIENT_LAYER, "no runtime");
    };
    let Ok(paths) = ClientRuntimePaths::resolve() else {
        return failed(CLIENT_LAYER, "no runtime directory");
    };
    let transport = Transport::for_alias(alias, &paths, SshOptions::default());
    let options = BootstrapOptions::default();
    match held.block_on(probe(&transport, options.probe_deadline)) {
        Ok(found) => probed(&found),
        Err(source) => failed(TRANSPORT_LAYER, &source.to_string()),
    }
}

/// A probe's answer as one object.
fn probed(found: &HostProbe) -> Value {
    object(vec![
        (
            "operating_system",
            text(&format!("{:?}", found.operating_system).to_lowercase()),
        ),
        (
            "architecture",
            text(&format!("{:?}", found.architecture).to_lowercase()),
        ),
        (
            "server",
            found.server.as_ref().map_or(Value::Null, |installed| {
                object(vec![
                    ("version", text(&installed.crate_version)),
                    (
                        "protocol_version",
                        Value::Whole(u64::from(installed.protocol_version)),
                    ),
                ])
            }),
        ),
        ("terminfo_installed", Value::Truth(found.terminfo_installed)),
        ("tic_available", Value::Truth(found.tic_available)),
        ("prefix", text(&found.prefix.display().to_string())),
    ])
}

/// The three sections that need the host to answer.
struct Answered {
    /// What its server says it is, with the tail of its own log.
    server: Value,
    /// What state it is in, and every state it went through to get there.
    state: Value,
    /// What a keystroke costs against it.
    round_trip: Value,
}

/// The three sections that need the host to answer: what its server is, what
/// state it is in with the transitions that got it there, and what a
/// keystroke costs.
fn held(alias: &str) -> Answered {
    // A socket that is not there is answered here rather than waited for: a
    // path with nothing at it is a fact, and one this program can state
    // without asking anybody.
    if let Some(socket) = alias.strip_prefix(LOCAL_PREFIX)
        && !Path::new(socket).exists()
    {
        let refused = failed(TRANSPORT_LAYER, &format!("there is no socket at {socket}"));
        return Answered {
            server: refused.clone(),
            state: object(vec![
                ("state", refused),
                ("went_through", Value::List(Vec::new())),
            ]),
            round_trip: skipped("the host was never reached"),
        };
    }
    let paths = match ClientRuntimePaths::resolve() {
        Ok(paths) => paths,
        Err(source) => return every(CLIENT_LAYER, &source.to_string()),
    };
    let artifacts = std::env::var_os(crate::ARTIFACTS_VARIABLE).map_or_else(
        || paths.directory.join(crate::ARTIFACTS_DIRECTORY),
        std::path::PathBuf::from,
    );
    if let Err(source) = std::fs::create_dir_all(&artifacts) {
        return every(CLIENT_LAYER, &source.to_string());
    }
    let manager = match HostManager::new(ManagerOptions::new(artifacts, paths)) {
        Ok(manager) => manager,
        Err(source) => return every(CLIENT_LAYER, &source.to_string()),
    };
    let events = manager.events();
    manager.add_host(alias);
    // Every state it went through on the way, which is the half of a
    // diagnosis that says whether it struggled or simply worked.
    let mut went = Vec::new();
    let mut reached = None;
    let Some(expires) = Instant::now().checked_add(REACH_DEADLINE) else {
        return every(CLIENT_LAYER, "no clock");
    };
    // Until it answers, until it fails once, or until the ceiling. The first
    // failure is where this stops rather than waiting out the retries: what
    // is being collected is what happened, and a host is tried again for ever
    // — a diagnosis that waited for that would never be written.
    let mut fell = None;
    while Instant::now() < expires && reached.is_none() && fell.is_none() {
        let left = expires.saturating_duration_since(Instant::now()).min(LOOK);
        let Ok(ManagerEvent::Moved { state, .. }) = events.recv_timeout(left) else {
            continue;
        };
        went.push(state.to_string());
        match state {
            HostState::Connected {
                server_version,
                upgrade,
            } => reached = Some((server_version, upgrade.is_some())),
            HostState::Failed { error, .. } => fell = Some(error),
            _otherwise => {}
        }
    }
    let transitions = Value::List(went.iter().map(|said| text(said)).collect());
    let Some((version, newer)) = reached else {
        let refused = failed(
            TRANSPORT_LAYER,
            &fell.unwrap_or_else(|| format!("{alias} never answered")),
        );
        return Answered {
            server: refused.clone(),
            state: object(vec![("state", refused), ("went_through", transitions)]),
            round_trip: skipped("the host was never reached"),
        };
    };
    let server = object(vec![
        ("version", text(&version)),
        ("upgrade_available", Value::Truth(newer)),
        ("log", logged(alias)),
    ]);
    let host_state = object(vec![
        ("state", text("connected")),
        ("went_through", transitions),
    ]);
    let round_trip = match measured(&manager, &events, alias, KEYSTROKES) {
        Ok(taken) => shaped(alias, &taken),
        Err((layer, detail)) => failed(layer, &detail),
    };
    Answered {
        server,
        state: host_state,
        round_trip,
    }
}

/// The same refusal in all three sections, for what stops all three.
fn every(layer: &str, detail: &str) -> Answered {
    let refused = failed(layer, detail);
    Answered {
        server: refused.clone(),
        state: refused,
        round_trip: skipped("nothing could be asked of the host"),
    }
}

/// The tail of the daemon's own log.
///
/// On this machine it is beside the socket; on a host it is read there, with
/// the same rule for where a daemon keeps its runtime that put it there.
fn logged(alias: &str) -> Value {
    match alias.strip_prefix(LOCAL_PREFIX) {
        Some(socket) => beside(Path::new(socket)),
        None => yonder(alias),
    }
}

/// The tail of a log beside a socket on this machine.
fn beside(socket: &Path) -> Value {
    let Some(directory) = socket.parent() else {
        return failed(CLIENT_LAYER, "the socket has no directory");
    };
    let path = directory.join(LOG_NAME);
    match std::fs::read_to_string(&path) {
        Ok(said) => tailed(&said),
        Err(source) => failed(CLIENT_LAYER, &format!("{}: {source}", path.display())),
    }
}

/// The tail of a log on a host, read there.
fn yonder(alias: &str) -> Value {
    let Ok(held) = runtime() else {
        return failed(CLIENT_LAYER, "no runtime");
    };
    let Ok(paths) = ClientRuntimePaths::resolve() else {
        return failed(CLIENT_LAYER, "no runtime directory");
    };
    let transport = Transport::for_alias(alias, &paths, SshOptions::default());
    let asked =
        format!("{LOG_SCRIPT}\ntail -n {LOG_LINES} \"$runtime/{LOG_NAME}\" 2>/dev/null || true");
    // Called inside the runtime and not merely awaited in it: this one is
    // not an `async fn`, so it spawns the child where it is called, and a
    // call from outside a runtime is a panic rather than an error.
    let deadline = BootstrapOptions::default().command_deadline;
    let read = held.block_on(async { RunsRemotely::run(&transport, &asked, deadline).await });
    match read {
        Ok(said) => tailed(&said),
        Err(source) => failed(TRANSPORT_LAYER, &source.to_string()),
    }
}

/// The last lines of some text, oldest first.
fn tailed(said: &str) -> Value {
    let lines: Vec<&str> = said.lines().collect();
    Value::List(
        lines
            .iter()
            .rev()
            .take(LOG_LINES)
            .rev()
            .map(|line| text(line))
            .collect(),
    )
}
