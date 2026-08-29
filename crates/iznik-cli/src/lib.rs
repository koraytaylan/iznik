//! Developer plumbing: `probe`, `state`, `tail`, `benchmark`, `doctor` and `uninstall`, printing structured text and never drawing a screen.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod benchmark;
pub mod doctor;
pub mod output;
pub mod probe;
pub mod state;
pub mod tail;
pub mod uninstall;

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

/// The flag that asks a command what it takes.
pub const HELP_FLAG: &str = "--help";

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, or a flag it cannot parse. Two, the conventional usage-error
/// status, so that a failed run's one and a refused command line are told
/// apart.
pub const USAGE_EXIT_CODE: u8 = 2;

/// Whether a command line asks what a command takes rather than asking it to
/// do the work.
///
/// Anywhere in the line: `iznik tail host0 --help` is the same question as
/// `iznik tail --help`, and a person who reaches for the flag late should not
/// have to reach for it again earlier.
#[must_use]
pub fn asked_for_help(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| argument == HELP_FLAG)
}

/// A command's usage line on standard output, and success.
///
/// The one line this binary writes that is not a JSON object, and the only
/// one that is an answer rather than an account of a host: a refusal carries
/// the same words to standard error wrapped as an object, because a script
/// reading a failure wants a field and a person asking a question wants a
/// line.
#[must_use]
pub fn help_with(usage: &str) -> ExitCode {
    let mut writing = std::io::stdout();
    // Written and flushed like everything else here, and a write nobody took
    // is not a success: `iznik probe --help | head -0` has nobody to answer.
    if writeln!(writing, "{usage}")
        .and_then(|()| writing.flush())
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// The layer a failure of this program's own is from.
///
/// Every refusal these commands print names one, because the first question
/// anybody asks about a system this tall is which part of it broke — and a
/// failure that is this program's own is the one layer nothing else can name.
pub const CLIENT_LAYER: &str = "client";

/// The layer a failure of the link is from.
pub const TRANSPORT_LAYER: &str = "transport";

/// A runtime for the commands that wait on a host.
///
/// Multi-threaded and its own: these commands own the process, so nothing is
/// gained by asking a caller for one.
///
/// # Errors
///
/// When the operating system will not give the threads.
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Where this program looks for the servers it may install on a host, under
/// its own runtime directory.
pub const ARTIFACTS_DIRECTORY: &str = "artifacts";

/// What names that directory instead, when a build puts it somewhere else.
///
/// Every command here but `probe` reaches a host through a bootstrap, and a
/// bootstrap installs the server this build carries — so where that server is
/// has to be sayable. A native application says it in its configuration; this
/// is a program run from a shell, so it says it the way a program run from a
/// shell says anything.
pub const ARTIFACTS_VARIABLE: &str = "IZNIK_ARTIFACTS_DIRECTORY";

/// A manager holding one host, waited for until it is connected.
///
/// Every command that asks a host anything needs the same four things, and
/// the same wait: a manager, a directory to find artifacts in, the host
/// added, and the host answering. What comes back is the manager and the
/// events it has not yet said, because a caller that took the events later
/// would have missed what happened while it was starting.
///
/// # Errors
///
/// The layer that refused and what it said.
pub fn holding(
    alias: &str,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<
    (
        iznik_client::host::manager::HostManager,
        std::sync::mpsc::Receiver<iznik_client::host::manager::ManagerEvent>,
    ),
    (&'static str, String),
> {
    use iznik_client::bootstrap::launch::BOOTSTRAP_DEADLINE;
    use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
    use iznik_client::host::state::HostState;
    use iznik_client::transport::ClientRuntimePaths;

    let paths =
        ClientRuntimePaths::resolve().map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let artifacts = std::env::var_os(ARTIFACTS_VARIABLE).map_or_else(
        || paths.directory.join(ARTIFACTS_DIRECTORY),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&artifacts).map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let manager = HostManager::new(ManagerOptions::new(artifacts, paths))
        .map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let events = manager.events();
    manager.add_host(alias);
    let expires = std::time::Instant::now()
        .checked_add(BOOTSTRAP_DEADLINE)
        .ok_or((CLIENT_LAYER, "no clock".to_owned()))?;
    // What the host last said went wrong, which is what a deadline that
    // passes should say rather than that nothing was heard: a host is tried
    // again after every failure, so the failures on the way are the story.
    let mut last = None;
    while std::time::Instant::now() < expires && !stop.load(std::sync::atomic::Ordering::Acquire) {
        let left = expires
            .saturating_duration_since(std::time::Instant::now())
            .min(LOOK);
        match events.recv_timeout(left) {
            Ok(ManagerEvent::Moved {
                state: HostState::Connected { .. },
                ..
            }) => return Ok((manager, events)),
            // Not an ending: a host that could not be reached is tried again,
            // and the harness that models this waits through exactly this.
            Ok(ManagerEvent::Moved {
                state: HostState::Failed { error, .. },
                ..
            }) => last = Some(error),
            Ok(ManagerEvent::Removed { .. }) => {
                return Err((
                    TRANSPORT_LAYER,
                    last.unwrap_or_else(|| format!("{alias} was given up on")),
                ));
            }
            Ok(_otherwise) => {}
            // A timeout is the look coming round again, not an ending.
            Err(_nothing) => {}
        }
    }
    Err((
        TRANSPORT_LAYER,
        last.unwrap_or_else(|| format!("{alias} never answered")),
    ))
}

/// How long a wait looks before it checks whether it has been interrupted.
const LOOK: std::time::Duration = std::time::Duration::from_millis(100);

/// The one host argument a subcommand takes, or nothing.
#[must_use]
pub fn one_host(arguments: &[OsString]) -> Option<String> {
    let mut rest = arguments.iter().skip(1);
    let named = rest.next()?.to_str()?.to_owned();
    rest.next().is_none().then_some(named)
}
