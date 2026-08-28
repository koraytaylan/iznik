//! `iznik-server --stdio`: the bridge between the standard streams and the
//! daemon's socket, starting the daemon on first use.
//!
//! This is the only command the bootstrap will ever run on a host. Everything
//! about finding the daemon or starting it lives here, which is why plan
//! 0005's bootstrap needs to know nothing about runtime directories, locks or
//! sockets: it runs one command over SSH and speaks `iznik/1` to it.
//!
//! It speaks nothing itself. Bytes go both ways untouched until either side
//! closes, so a relay that is working is a relay that is invisible.

use std::ffi::OsString;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::daemon::idle::SOCKET_POLL_INTERVAL;
use crate::daemon::{DaemonOptions, RuntimePaths};

/// The exit status of a relay that could not reach a daemon.
const FAILED: u8 = 1;

/// The subcommand this module answers to.
const STDIO: &str = "--stdio";

/// Writes a line to standard error, through the runtime: this binary never
/// blocks on the standard library's streams, and its standard output belongs
/// to the client it is relaying for.
async fn complain(line: &str) {
    let mut stderr = tokio::io::stderr();
    let _written = stderr.write_all(format!("{line}\n").as_bytes()).await;
    let _flushed = stderr.flush().await;
}

/// Connects to the daemon, starting one and waiting for it if there is none.
///
/// # Errors
///
/// Words for a person when the daemon cannot be started or never answers.
async fn reach(paths: &RuntimePaths, cap: Duration) -> Result<UnixStream, String> {
    if let Ok(stream) = UnixStream::connect(&paths.socket).await {
        return Ok(stream);
    }
    let Ok(program) = std::env::current_exe() else {
        return Err("this binary's own path could not be found".to_owned());
    };
    // The same start a person would run, so there is one way a daemon begins.
    let started = tokio::process::Command::new(program)
        .arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    match started {
        Err(error) => return Err(format!("the daemon could not be started: {error}")),
        // Waiting out the cap for a daemon that has already failed adds ten
        // seconds to a bootstrap and throws away the reason.
        Ok(status) if !status.success() => {
            return Err(format!(
                "the daemon exited with {status} rather than starting; see {}",
                paths.log.display()
            ));
        }
        Ok(_started) => {}
    }
    let waited = Instant::now();
    while waited.elapsed() < cap {
        if let Ok(stream) = UnixStream::connect(&paths.socket).await {
            return Ok(stream);
        }
        tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
    }
    Err(format!(
        "no daemon answered on {} within {cap:?}",
        paths.socket.display()
    ))
}

/// Copies both ways until the daemon closes, which is when there is nothing
/// left to carry.
///
/// The two directions are not symmetric, and the asymmetry is the whole of it.
/// A client that has said all it will is not gone: it is waiting for an
/// answer, so its end is a half-close — the daemon is told, and what it says
/// back is still carried. A daemon that has gone leaves nothing to answer
/// with, so that end is the relay's end, whether or not the client is still
/// holding its terminal open.
///
/// # Errors
///
/// Words for a person when the copy fails partway.
async fn relay(stream: UnixStream) -> Result<(), String> {
    let (mut from_daemon, mut to_daemon) = stream.into_split();
    let outward = tokio::spawn(async move {
        let mut input = tokio::io::stdin();
        let _carried = tokio::io::copy(&mut input, &mut to_daemon).await;
        let _closed = to_daemon.shutdown().await;
    });
    let mut output = tokio::io::stdout();
    let carried = tokio::io::copy(&mut from_daemon, &mut output).await;
    // Whatever the daemon said last is the client's, even though the relay is
    // on its way out.
    let _flushed = output.flush().await;
    outward.abort();
    carried
        .map(|_count| ())
        .map_err(|error| format!("the relay stopped: {error}"))
}

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first. This takes no flags of its own: what it does is decided
/// by whether a daemon is there.
pub async fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.first().and_then(|argument| argument.to_str()) != Some(STDIO) {
        complain("usage: iznik-server --stdio").await;
        return ExitCode::from(crate::USAGE_EXIT_CODE);
    }
    if arguments.len() > 1 {
        complain("--stdio takes no arguments").await;
        return ExitCode::from(crate::USAGE_EXIT_CODE);
    }
    let paths = match RuntimePaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            complain(&error.to_string()).await;
            return ExitCode::from(FAILED);
        }
    };
    let cap = DaemonOptions::default().socket_ready_cap;
    let stream = match reach(&paths, cap).await {
        Ok(stream) => stream,
        Err(refusal) => {
            complain(&refusal).await;
            return ExitCode::from(FAILED);
        }
    };
    match relay(stream).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            complain(&refusal).await;
            ExitCode::from(FAILED)
        }
    }
}
