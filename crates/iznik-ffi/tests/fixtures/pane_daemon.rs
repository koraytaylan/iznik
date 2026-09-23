//! A daemon on this machine, a client of the boundary's reaching it, and a
//! session with one pane on it — what every case about a pane's bytes stands
//! on.

use core::time::Duration;
use std::ffi::CString;
use std::path::PathBuf;
use std::time::Instant;

use iznik::error::{Error, Layer, OK};
use iznik::pane::iznik_pane_input;
use iznik::{Client, Configuration, iznik_client_new, iznik_command, iznik_host_add};
use iznik_protocol::command::{SessionCommand, encode_session_command};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

use super::Failed;

/// How long a case waits for something that should happen at once.
pub(super) const PROMPT: Duration = Duration::from_secs(10);

/// The width a pane is made at.
pub(super) const COLUMNS: u16 = 80;

/// Its height.
pub(super) const ROWS: u16 = 24;

/// The pane every case attaches to.
pub(super) const PANE: u64 = 1;

/// A temporary directory of this case's own, removed when the guard drops.
#[derive(Debug)]
pub(super) struct Scratch {
    /// Where it is.
    pub(super) path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When it cannot be made.
pub(super) fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-pipe-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the daemon a case stands up.
///
/// # Errors
///
/// When it cannot be built.
pub(super) fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// An error struct to be filled in.
pub(super) fn blank() -> Error {
    Error {
        code: OK,
        layer: Layer::Client,
        message: core::ptr::null(),
    }
}

/// What an error says.
pub(super) fn said(error: &Error) -> String {
    if error.message.is_null() {
        return String::new();
    }
    // SAFETY: iznik's own promise: valid until the next call on this thread.
    unsafe { core::ffi::CStr::from_ptr(error.message) }
        .to_string_lossy()
        .into_owned()
}

/// A client, a daemon, and a session with one pane on it.
///
/// # Errors
///
/// When any of the three will not come up.
pub(super) fn connected(
    held: &Scratch,
    runtime: &Runtime,
) -> Result<(Stack, *mut Client, CString), Failed> {
    let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
    let directory = CString::new(held.path.join("runtime").display().to_string())?;
    let configuration = Configuration {
        runtime_directory: directory.as_ptr(),
        artifacts_directory: core::ptr::null(),
        askpass_program: core::ptr::null(),
        log_path: core::ptr::null(),
    };
    let mut error = blank();
    // SAFETY: the configuration is alive for this call and its string is
    // null-terminated.
    let client = unsafe { iznik_client_new(&raw const configuration, &raw mut error) };
    if client.is_null() {
        return Err(format!("no client: {}", said(&error)).into());
    }
    let alias = CString::new(format!("unix:{}", stack.socket().display()))?;
    // SAFETY: the client is live and the alias null-terminated.
    let added = unsafe { iznik_host_add(client, alias.as_ptr(), &raw mut error) };
    if added != OK {
        return Err(format!("the host was not taken: {}", said(&error)).into());
    }
    make_a_session(client, &alias)?;
    Ok((stack, client, alias))
}

/// Makes a session, and waits until the host has one.
///
/// # Errors
///
/// When the command will not go, or the session never appears.
pub(super) fn make_a_session(client: *mut Client, alias: &CString) -> Result<(), Failed> {
    let asked = encode_session_command(&SessionCommand::CreateSession {
        name: "work".to_owned(),
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
    })?;
    let mut error = blank();
    let mut number: u64 = 0;
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        // SAFETY: every pointer is alive for the call and the bytes are this
        // case's own.
        let sent = unsafe {
            iznik_command(
                client,
                alias.as_ptr(),
                asked.as_ptr(),
                asked.len(),
                &raw mut number,
                &raw mut error,
            )
        };
        if sent == OK {
            // The pane exists once the host has answered; the attach below
            // waits for what the host says about it.
            std::thread::sleep(Duration::from_millis(200));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("no session: {}", said(&error)).into())
}

/// Types one line into a pane.
///
/// # Errors
///
/// When the boundary refuses it.
pub(super) fn typed(
    client: *mut Client,
    alias: &CString,
    pane: u64,
    text: &str,
) -> Result<(), Failed> {
    let bytes = text.as_bytes();
    let mut error = blank();
    // SAFETY: every pointer is alive for the call.
    let sent = unsafe {
        iznik_pane_input(
            client,
            alias.as_ptr(),
            pane,
            bytes.as_ptr(),
            bytes.len(),
            &raw mut error,
        )
    };
    if sent == OK {
        return Ok(());
    }
    Err(format!("the input was refused: {}", said(&error)).into())
}
