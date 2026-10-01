//! Replacing the running daemon with another binary of itself, in place.
//!
//! The process stops taking clients, writes what it holds, clears
//! close-on-exec on the listener, the lock and every master, and `exec`s the
//! staged binary. There is no moment when two processes own those descriptors,
//! and a failed `exec` is still this process, still holding them.

use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use iznik_protocol::identity::PaneId;
use nix::unistd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{RwLock, mpsc};

use crate::adopt::{
    ADOPTED_STATE_VERSION, AdoptError, AdoptedPane, AdoptedState, TermiosState, read_state,
    write_state,
};
use crate::daemon::lock::Lock;
use crate::daemon::paths::RuntimePaths;
use crate::daemon::socket::{self, Listener};
use crate::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use crate::pty::program::PROGRAM_INTERVAL;
use crate::pty::spawn::{Program, PtyProcess};
use crate::session::registry::{Registry, RegistryDefaults};
use crate::terminal::mirror::MirrorThread;

/// The longest path a request may name.
const MAXIMUM_PATH_BYTES: usize = 4_096;

/// One request to replace this process with `binary`.
#[derive(Debug)]
pub struct Request {
    /// The staged binary to exec.
    pub binary: PathBuf,
    /// Where a refusal is written. A success never writes: the process is gone.
    pub stream: socket::Stream,
}

/// The requests, and what an accepted replacement needs.
#[derive(Debug)]
pub struct Door {
    /// Requests from [`watch`].
    pub requests: mpsc::UnboundedReceiver<Request>,
    /// The listening socket's descriptor.
    pub listener: RawFd,
    /// The lock's descriptor.
    pub lock: RawFd,
    /// Where the record is written.
    pub paths: RuntimePaths,
    /// Whether requests are still being accepted.
    pub open: bool,
}

/// Accepts replacement requests on `listener` until it closes.
pub fn watch(listener: Listener) -> mpsc::UnboundedReceiver<Request> {
    let (sender, requests) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _address)) = listener.accept().await else {
                break;
            };
            let Ok(binary) = read_path(&mut stream).await else {
                continue;
            };
            if sender.send(Request { binary, stream }).is_err() {
                break;
            }
        }
    });
    requests
}

/// The path a request wrote, one line.
///
/// # Errors
///
/// [`AdoptError::Io`] when the request is not a path.
async fn read_path(stream: &mut socket::Stream) -> Result<PathBuf, AdoptError> {
    let mut bytes = Vec::new();
    let mut one = [0; 1];
    loop {
        let read = stream
            .read(&mut one)
            .await
            .map_err(|source| AdoptError::Io {
                detail: format!("reading a replacement request: {source}"),
            })?;
        if read == 0 {
            break;
        }
        let Some(byte) = one.first().copied() else {
            break;
        };
        if byte == b'\n' {
            break;
        }
        bytes.push(byte);
        if bytes.len() > MAXIMUM_PATH_BYTES {
            break;
        }
    }
    let path = String::from_utf8(bytes).map_err(|_error| AdoptError::Io {
        detail: "a replacement request was not a path".to_owned(),
    })?;
    Ok(PathBuf::from(path.trim()))
}

/// Performs `request` or writes why it did not. A success does not return.
///
/// # Errors
///
/// [`AdoptError`] when the replacement does not happen. A success execs and
/// does not return.
pub async fn perform(
    mut request: Request,
    registry: &RwLock<Registry>,
    listener: RawFd,
    lock: RawFd,
    paths: &RuntimePaths,
) -> Result<(), AdoptError> {
    let outcome = replace(&request.binary, registry, listener, lock, paths).await;
    if let Err(error) = &outcome {
        let _wrote = request
            .stream
            .write_all(format!("{error}\n").as_bytes())
            .await;
        let _flushed = request.stream.flush().await;
    }
    outcome
}

/// Writes the record and execs `binary`.
///
/// # Errors
///
/// [`AdoptError`] when the record cannot be written or the binary cannot be
/// executed. A success does not return.
async fn replace(
    binary: &Path,
    registry: &RwLock<Registry>,
    listener: RawFd,
    lock: RawFd,
    paths: &RuntimePaths,
) -> Result<(), AdoptError> {
    let held = registry.read().await;
    let state = match snapshot(&held).await {
        Ok(state) => state,
        Err(error) => {
            resume_panes(&held);
            return Err(error);
        }
    };
    if let Err(error) = exec_staged(binary, &state, listener, lock, paths) {
        resume_panes(&held);
        return Err(error);
    }
    Ok(())
}

/// Clears close-on-exec, writes the record and its copy, and execs `binary`.
///
/// # Errors
///
/// [`AdoptError`] when a descriptor is not open, the record cannot be written,
/// or the binary cannot be executed. A success does not return.
fn exec_staged(
    binary: &Path,
    state: &AdoptedState,
    listener: RawFd,
    lock: RawFd,
    paths: &RuntimePaths,
) -> Result<(), AdoptError> {
    for pane in &state.panes {
        keep(pane.descriptor)?;
    }
    keep(listener)?;
    keep(lock)?;
    write_state(&paths.state, state)?;
    let kept = suffixed(&paths.state, ".kept");
    std::fs::copy(&paths.state, &kept).map_err(|source| AdoptError::Io {
        detail: format!("keeping a copy of the adoption record: {source}"),
    })?;
    exec_binary(binary, &paths.state, listener, lock)
}

/// Lets every pane read its terminal again.
fn resume_panes(registry: &Registry) {
    for session in &registry.snapshot().sessions {
        for tab in &session.tabs {
            for described in &tab.panes {
                if let Some(pane) = registry.pane(described.id) {
                    pane.resume_output();
                }
            }
        }
    }
}

/// The record of what `registry` holds right now.
///
/// Each pane's reader is stopped first, and the ring is copied under one lock.
/// A pane that cannot be copied fails the whole record: an empty ring is not
/// substituted for a range the copy missed.
///
/// # Errors
///
/// [`AdoptError::Ended`] when a pane, its master, or its mirror is gone.
async fn snapshot(registry: &Registry) -> Result<AdoptedState, AdoptError> {
    let model = registry.snapshot();
    let mut panes = Vec::new();
    for session in &model.sessions {
        for tab in &session.tabs {
            for described in &tab.panes {
                match carried(registry, described.id).await {
                    Ok(pane) => panes.push(pane),
                    Err(error) => {
                        resume_panes(registry);
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok(AdoptedState {
        version: ADOPTED_STATE_VERSION,
        instance: registry.instance(),
        model,
        panes,
    })
}

/// One pane's carried record.
///
/// # Errors
///
/// [`AdoptError::Ended`] when the pane, its master, or its mirror is gone.
async fn carried(registry: &Registry, pane: PaneId) -> Result<AdoptedPane, AdoptError> {
    let held = registry.pane(pane).ok_or(AdoptError::Ended { pane })?;
    let descriptor = held.master_descriptor().ok_or(AdoptError::Ended { pane })?;
    let ring = held
        .carried_ring()
        .await
        .map_err(|_error| AdoptError::Ended { pane })?;
    Ok(AdoptedPane {
        pane,
        descriptor,
        process_id: held.process_id(),
        sequence: ring.newest,
        ring: ring.bytes,
        termios: TermiosState {
            bytes: iznik::termios_image(descriptor),
        },
    })
}

/// Clears close-on-exec, or refuses a descriptor that is not open.
///
/// # Errors
///
/// [`AdoptError::Master`] when `descriptor` is not open, and
/// [`AdoptError::Io`] when the flag cannot be cleared.
fn keep(descriptor: RawFd) -> Result<(), AdoptError> {
    iznik::keep_across_exec(descriptor).map_err(|error| {
        if error.detail.contains("not open") {
            AdoptError::Master { descriptor }
        } else {
            AdoptError::Io {
                detail: error.detail,
            }
        }
    })
}

/// Execs `binary` as `--adopt`. On failure the previous binary is put back
/// and this process is still the daemon.
///
/// # Errors
///
/// [`AdoptError::Execute`] when the binary cannot be executed, and
/// [`AdoptError::Io`] when its path cannot be an argument.
fn exec_binary(
    binary: &Path,
    state: &Path,
    listener: RawFd,
    lock: RawFd,
) -> Result<(), AdoptError> {
    let program = binary_text(binary)?;
    let arguments = arguments_of(binary, state, listener, lock)?;
    match unistd::execv(&program, &arguments) {
        Ok(impossible) => match impossible {},
        Err(error) => {
            restore_previous(binary);
            Err(AdoptError::Execute {
                path: binary.to_path_buf(),
                detail: error.to_string(),
            })
        }
    }
}

/// The argument vector of an `--adopt` invocation, including `argv[0]`.
///
/// # Errors
///
/// [`AdoptError::Io`] when a path or argument contains a nul.
fn arguments_of(
    binary: &Path,
    state: &Path,
    listener: RawFd,
    lock: RawFd,
) -> Result<Vec<CString>, AdoptError> {
    let mut arguments = Vec::new();
    for text in [
        binary_text(binary)?,
        text_of("--adopt")?,
        binary_text(state)?,
        text_of(&listener.to_string())?,
        text_of(&lock.to_string())?,
    ] {
        arguments.push(text);
    }
    Ok(arguments)
}

/// `path` as a `CString`.
///
/// # Errors
///
/// [`AdoptError::Io`] when `path` contains a nul.
fn binary_text(path: &Path) -> Result<CString, AdoptError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_error| AdoptError::Io {
        detail: format!("{} is not a path an exec can take", path.display()),
    })
}

/// `text` as a `CString`.
///
/// # Errors
///
/// [`AdoptError::Io`] when `text` contains a nul.
fn text_of(text: &str) -> Result<CString, AdoptError> {
    CString::new(text).map_err(|_error| AdoptError::Io {
        detail: format!("{text} cannot be an argument"),
    })
}

/// Copies the kept record back over `state`, when the replacement wrote one.
fn restore_record(state: &Path) {
    let kept = suffixed(state, ".kept");
    if kept.is_file() {
        let _copied = std::fs::copy(&kept, state);
    }
}

/// Copies `binary.previous` back over `binary`, and the digest beside it.
fn restore_previous(binary: &Path) {
    let previous = suffixed(binary, ".previous");
    if previous.is_file() {
        let _copied = std::fs::copy(&previous, binary);
    }
    let digest = suffixed(binary, ".sha256");
    let saved = suffixed(&digest, ".previous");
    if saved.is_file() {
        let _copied = std::fs::copy(&saved, &digest);
    }
}

/// `adopt.refuse` beside the state file.
fn refusal_marker(state: &Path) -> PathBuf {
    state.with_file_name("adopt.refuse")
}

/// `path` with `suffix` appended to its file name.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map_or_else(String::new, |name| {
        format!("{}{suffix}", name.to_string_lossy())
    });
    path.with_file_name(name)
}

/// `--adopt`: own the inherited listener and lock, rebuild the registry, serve.
pub async fn adopted(arguments: &[OsString]) -> ExitCode {
    let Some(parsed) = parse_adopted(arguments) else {
        super::complain("usage: iznik-server --adopt <state> <listener> <lock>").await;
        return ExitCode::from(super::FAILED);
    };
    if refusal_marker(&parsed.state).is_file() {
        let _removed = std::fs::remove_file(refusal_marker(&parsed.state));
        return rollback(&parsed).await;
    }
    match serve_adopted(&parsed).await {
        Ok(status) => status,
        Err(error) => {
            tracing::error!(%error, "adoption refused");
            super::complain(&error.to_string()).await;
            rollback(&parsed).await
        }
    }
}

/// The three arguments after `--adopt`.
struct ParsedAdopt {
    /// The record.
    state: PathBuf,
    /// The inherited listener.
    listener: RawFd,
    /// The inherited lock.
    lock: RawFd,
}

/// Where `--adopt` places the inherited listener, after the state path.
const LISTENER_ARGUMENT: usize = 2;
/// Where `--adopt` places the inherited lock, after the listener.
const LOCK_ARGUMENT: usize = 3;

/// Parses `--adopt`'s arguments, or nothing when they are not three numbers
/// and a path.
fn parse_adopted(arguments: &[OsString]) -> Option<ParsedAdopt> {
    let state = arguments.get(1)?.clone();
    let listener = parse_descriptor(arguments.get(LISTENER_ARGUMENT)?)?;
    let lock = parse_descriptor(arguments.get(LOCK_ARGUMENT)?)?;
    Some(ParsedAdopt {
        state: PathBuf::from(state),
        listener,
        lock,
    })
}

/// A descriptor number.
fn parse_descriptor(text: &OsString) -> Option<RawFd> {
    text.to_string_lossy().parse().ok()
}

/// Rebuilds the daemon from `parsed` and serves on the inherited listener.
///
/// # Errors
///
/// [`AdoptError`] when the record, a master, or the inherited listener cannot
/// be taken. The caller rolls back.
async fn serve_adopted(parsed: &ParsedAdopt) -> Result<ExitCode, AdoptError> {
    let paths = RuntimePaths::resolve().map_err(|error| AdoptError::Io {
        detail: error.to_string(),
    })?;
    // Decode and check every master before owning the listener or the lock.
    // A refusal then still has those descriptors to hand to the previous binary.
    let state = read_state(&parsed.state)?;
    for pane in &state.panes {
        keep(pane.descriptor)?;
    }
    let listener = inherited_listener(parsed.listener)?;
    let lock = match inherited_lock(&paths, parsed.lock) {
        Ok(lock) => lock,
        Err(error) => {
            hold_open(listener);
            return Err(error);
        }
    };
    let mut processes = BTreeMap::new();
    for pane in &state.panes {
        match PtyProcess::adopt(pane.descriptor, pane.process_id) {
            Ok(process) => {
                processes.insert(pane.pane, process);
            }
            Err(error) => {
                release_for_rollback(listener, lock, processes);
                return Err(adopt_process(&error, pane.descriptor));
            }
        }
    }
    let executable = std::env::current_exe().ok();
    let mirrors = match MirrorThread::start() {
        Ok(thread) => thread,
        Err(error) => {
            release_for_rollback(listener, lock, processes);
            return Err(AdoptError::Io {
                detail: error.to_string(),
            });
        }
    };
    let registry = match Registry::adopt(
        RegistryDefaults {
            program: Program::LoginShell,
            terminfo_directory: executable
                .as_deref()
                .and_then(crate::daemon::terminfo_beside),
            agent_socket: Some(paths.agent.clone()),
            program_interval: PROGRAM_INTERVAL,
        },
        std::sync::Arc::new(std::sync::Mutex::new(HistoryBudget::new(
            DEFAULT_HISTORY_BUDGET_BYTES,
        ))),
        mirrors,
        &state,
        processes,
    )
    .await
    {
        Ok(registry) => registry,
        Err(abandoned) => {
            let (error, remaining, partial) = abandoned;
            release_for_rollback(listener, lock, remaining);
            hold_open(partial);
            return Err(error);
        }
    };
    if let Err(error) = crate::daemon::logging::initialize(&paths.log).await {
        super::complain(&format!("the daemon runs without its log: {error}")).await;
    }
    let (_asked, shutdown) = tokio::sync::watch::channel(false);
    match super::run_with(
        &paths,
        super::DaemonOptions::default(),
        shutdown,
        listener,
        lock,
        Some(registry),
    )
    .await
    {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            super::complain(&error.to_string()).await;
            Ok(ExitCode::from(super::FAILED))
        }
    }
}

/// The inherited listener, as the runtime's socket type.
///
/// # Errors
///
/// [`AdoptError::Io`] when `descriptor` is not an open listener.
fn inherited_listener(descriptor: RawFd) -> Result<Listener, AdoptError> {
    let listener = iznik::adopt_listener(descriptor).map_err(|error| AdoptError::Io {
        detail: error.detail,
    })?;
    Listener::from_std(listener).map_err(|source| AdoptError::Io {
        detail: format!("the inherited listener: {source}"),
    })
}

/// The inherited lock file.
///
/// # Errors
///
/// [`AdoptError::Master`] when `descriptor` is not open, and
/// [`AdoptError::Io`] when it cannot be taken as the lock.
fn inherited_lock(paths: &RuntimePaths, descriptor: RawFd) -> Result<Lock, AdoptError> {
    let file = iznik::adopt_file(descriptor).map_err(|_error| AdoptError::Master { descriptor })?;
    Lock::inherit(paths.lock.clone(), file).map_err(|error| AdoptError::Io {
        detail: error.to_string(),
    })
}

/// Leaves `listener`, `lock` and every adopted master open for the next exec.
///
/// Dropping them would close the descriptors and release the lock, and the
/// previous binary would be handed numbers the kernel had already closed.
fn release_for_rollback(listener: Listener, lock: Lock, processes: BTreeMap<PaneId, PtyProcess>) {
    hold_open(listener);
    hold_open(lock);
    for (_pane, process) in processes {
        hold_open(process);
    }
}

/// Keeps `value` alive until this process is replaced.
///
/// The descriptors it owns stay open across the next `exec`. Dropping it would
/// close them first.
fn hold_open<Held>(value: Held) {
    let _kept = Box::leak(Box::new(value));
}

/// The adoption error a failed [`PtyProcess::adopt`] is.
fn adopt_process(error: &crate::pty::spawn::PtyError, descriptor: RawFd) -> AdoptError {
    if error.to_string().contains("not open") {
        AdoptError::Master { descriptor }
    } else {
        AdoptError::Io {
            detail: error.to_string(),
        }
    }
}

/// Puts the previous binary back and execs it with the same adoption arguments.
///
/// The record is restored from the copy written beside it first, so a record
/// this process refused — truncated, trailing, another version — is not what
/// the previous binary is asked to adopt.
async fn rollback(parsed: &ParsedAdopt) -> ExitCode {
    let Ok(current) = std::env::current_exe() else {
        return ExitCode::from(super::FAILED);
    };
    restore_record(&parsed.state);
    restore_previous(&current);
    let previous = suffixed(&current, ".previous");
    let Ok(arguments) = arguments_of(&previous, &parsed.state, parsed.listener, parsed.lock) else {
        return ExitCode::from(super::FAILED);
    };
    let Ok(program) = binary_text(&previous) else {
        return ExitCode::from(super::FAILED);
    };
    match unistd::execv(&program, &arguments) {
        Ok(impossible) => match impossible {},
        Err(error) => {
            super::complain(&format!(
                "could not execute {}: {error}",
                previous.display()
            ))
            .await;
            ExitCode::from(super::FAILED)
        }
    }
}

/// `--adopt-request <binary>`: ask the running daemon to exec `binary`.
pub async fn request(arguments: &[OsString]) -> ExitCode {
    let Some(binary) = arguments.get(1) else {
        super::complain("usage: iznik-server --adopt-request <binary>").await;
        return ExitCode::from(super::FAILED);
    };
    let paths = match RuntimePaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            super::complain(&error.to_string()).await;
            return ExitCode::from(super::FAILED);
        }
    };
    let outcome = ask(&paths, PathBuf::from(binary)).await;
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            super::complain(&error.to_string()).await;
            ExitCode::from(super::FAILED)
        }
    }
}

/// Writes `binary` to the daemon's adoption socket and reads a refusal, if
/// one comes back before the process is replaced.
///
/// # Errors
///
/// [`AdoptError::Io`] when the daemon cannot be asked, or answers with a refusal.
async fn ask(paths: &RuntimePaths, binary: PathBuf) -> Result<(), AdoptError> {
    let mut stream = tokio::net::UnixStream::connect(&paths.adopt)
        .await
        .map_err(|source| AdoptError::Io {
            detail: format!("connecting to {}: {source}", paths.adopt.display()),
        })?;
    stream
        .write_all(format!("{}\n", binary.display()).as_bytes())
        .await
        .map_err(|source| AdoptError::Io {
            detail: format!("asking for a replacement: {source}"),
        })?;
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .await
        .map_err(|source| AdoptError::Io {
            detail: format!("reading the replacement's answer: {source}"),
        })?;
    if answer.is_empty() {
        return Ok(());
    }
    Err(AdoptError::Io {
        detail: String::from_utf8_lossy(&answer).trim().to_owned(),
    })
}
