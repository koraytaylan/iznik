//! The `iznik-server` binary: a dispatcher that looks at the first argument only and hands the whole command line to the module that owns the subcommand.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

use iznik_server::{USAGE_EXIT_CODE, daemon, relay};
use tokio::io::AsyncWriteExt;

/// The flags this binary routes, in the order `--help` lists them.
const SUBCOMMANDS: &[&str] = &["--stdio", "--daemon", "--foreground", "--stop", "--version"];

/// Builds the runtime every entry point runs on, then dispatches. A runtime
/// that cannot be built is the one failure this binary cannot report — the
/// server never blocks on the standard library's streams, and there is no
/// runtime to report through — so it is a failure status and nothing else.
fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return ExitCode::FAILURE;
    };
    let relaying = arguments.first().and_then(|argument| argument.to_str()) == Some("--stdio");
    let status = runtime.block_on(dispatch(&arguments));
    // Only the relay. It reads standard input on a blocking thread, and a
    // client that has stopped writing leaves that read parked for ever:
    // dropping the runtime waits for every blocking call to return, so a relay
    // whose daemon had gone would hang holding a terminal open. It has flushed
    // what it had to say by here and has nothing else to finish.
    //
    // The daemon does. Its runtime is dropped, which waits for the blocking
    // work still in flight — a pane's reaper among it — so that what it was
    // doing on the way out is done rather than abandoned.
    if relaying {
        runtime.shutdown_background();
    }
    status
}

/// Routes by the first argument. The owning module receives every argument after
/// the program name, its subcommand first, and parses its own flags.
async fn dispatch(arguments: &[OsString]) -> ExitCode {
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("--stdio") => relay::run(arguments).await,
        Some("--daemon" | "--foreground" | "--stop" | "--version") => daemon::run(arguments).await,
        Some("--help") => help().await,
        _ => usage().await,
    }
}

/// The usage line: the program and every subcommand it knows.
fn usage_line() -> String {
    format!(
        "usage: iznik-server <{}> [arguments]",
        SUBCOMMANDS.join(" | ")
    )
}

/// `--help`: the usage line on standard output, and success.
async fn help() -> ExitCode {
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("{}\n", usage_line()).as_bytes())
        .await
        .unwrap_or_default();
    stdout.flush().await.unwrap_or_default();
    ExitCode::SUCCESS
}

/// A first argument the dispatcher does not know, or none: the usage line on
/// standard error, and the usage exit code.
async fn usage() -> ExitCode {
    let mut stderr = tokio::io::stderr();
    stderr
        .write_all(format!("{}\n", usage_line()).as_bytes())
        .await
        .unwrap_or_default();
    stderr.flush().await.unwrap_or_default();
    ExitCode::from(USAGE_EXIT_CODE)
}
