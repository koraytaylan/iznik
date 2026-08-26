//! `iznik-server --stdio`: the bridge between the standard streams and the daemon's socket, starting the daemon on first use.
//!
//! Filled by task `stdio-relay` of plan 0004; until then this module holds only its documentation and the stub of its entry point.

use std::ffi::OsString;
use std::process::ExitCode;

use tokio::io::AsyncWriteExt;

/// The subcommand's entry point: a stub until task `stdio-relay` replaces its body.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags. The server never
/// blocks on the standard library's streams, so even this line goes through
/// the runtime's standard error.
pub async fn run(arguments: &[OsString]) -> ExitCode {
    let subcommand = arguments.first().map_or_else(String::new, |argument| {
        argument.to_string_lossy().into_owned()
    });
    let mut stderr = tokio::io::stderr();
    stderr
        .write_all(format!("{subcommand}: not implemented until task stdio-relay\n").as_bytes())
        .await
        .unwrap_or_default();
    stderr.flush().await.unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
