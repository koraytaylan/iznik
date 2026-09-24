//! What a C application is told when a host's link goes: which command's
//! outcome is unknown, and which pane's keystrokes were dropped — in the
//! fields it reads them from, not only in words.
//!
//! The host is scripted on a socket on this machine: it answers the
//! handshake, and the first command it is sent it takes the link down with,
//! unanswered, as a host that dies mid-command does. It takes no second
//! connection, so the host stays without a link while the case types.

use core::ffi::c_void;
use core::time::Duration;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use iznik::error::{Error, Layer, OK};
use iznik::model::{Event, EventKind};
use iznik::pane::iznik_pane_input;
use iznik::{
    Client, Configuration, iznik_client_free, iznik_client_new, iznik_command, iznik_host_add,
    iznik_set_event_callback,
};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{SessionCommand, encode_session_command};
use iznik_protocol::identity::Generation;
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, encode_host_model};
use tokio::net::UnixListener;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How often it looks.
const LOOK: Duration = Duration::from_millis(10);

/// The pane the keystrokes are for: any number, so long as it is not zero.
const PANE: u64 = 7;

/// The width a session is asked for at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// One notification, as the case kept it.
#[derive(Debug)]
struct Told {
    /// What kind it was.
    kind: EventKind,
    /// The pane it named, or zero.
    pane: u64,
    /// The command it named, or zero.
    command: u64,
}

/// A temporary directory of this case's own, removed when the guard drops.
struct Scratch {
    /// Where it is.
    path: PathBuf,
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
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-ffi-lost-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// The callback: every notification and snapshot, with the fields this case
/// reads.
extern "C" fn record(event: *const Event, context: *mut c_void) {
    if event.is_null() || context.is_null() {
        return;
    }
    // SAFETY: iznik hands a live event, valid for the length of this call.
    let held = unsafe { &*event };
    if !matches!(held.kind, EventKind::Notification | EventKind::Snapshot) {
        return;
    }
    // SAFETY: the context is this case's own box, which outlives the client.
    let seen = unsafe { &*context.cast::<Mutex<Vec<Told>>>() };
    if let Ok(mut kept) = seen.lock() {
        kept.push(Told {
            kind: held.kind,
            pane: held.pane,
            command: held.command_id,
        });
    }
}

/// Serves one connection: the handshake, the model, and — at the first
/// command — the link taken down with the command unanswered.
async fn one_connection(stream: tokio::net::UnixStream) {
    let mut link = FramedLink::new(stream);
    loop {
        let asked = {
            let Ok(Some(frame)) = link.next_frame().await else {
                return;
            };
            decode_to_server(frame.payload)
        };
        let said = match asked {
            Ok(ToServer::Hello { .. }) => ToClient::Hello {
                protocol_version: PROTOCOL_VERSION,
                server_version: "scripted".to_owned(),
                capabilities: Capabilities::from_bits(0),
                instance: None,
                build: None,
            },
            Ok(ToServer::SnapshotRequest) => {
                let model = HostModel {
                    generation: Generation(1),
                    sessions: Vec::new(),
                };
                let Ok(payload) = encode_host_model(&model) else {
                    return;
                };
                ToClient::Snapshot {
                    generation: model.generation,
                    payload,
                }
            }
            // Gone with the command unanswered.
            Ok(ToServer::Command { .. }) | Err(_) => return,
            Ok(_otherwise) => continue,
        };
        let Ok(bytes) = encode_to_client(&said) else {
            return;
        };
        if link.send(CHANNEL_CONTROL, &bytes).await.is_err() {
            return;
        }
    }
}

/// A host that takes one connection and no other.
///
/// # Errors
///
/// When the socket cannot be bound.
fn scripted(runtime: &Runtime, socket: &std::path::Path) -> Result<(), Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let _serving = runtime.spawn(async move {
        if let Ok((stream, _from)) = listener.accept().await {
            drop(listener);
            one_connection(stream).await;
        }
    });
    Ok(())
}

/// An error struct to be filled in.
fn blank() -> Error {
    Error {
        code: OK,
        layer: Layer::Client,
        message: core::ptr::null(),
    }
}

/// Waits until what has been told satisfies `wanted`.
///
/// # Errors
///
/// When it never does.
fn await_told(
    seen: *mut Mutex<Vec<Told>>,
    what: &str,
    wanted: impl Fn(&[Told]) -> bool,
) -> Result<(), Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        // SAFETY: the case's own box, alive until the case ends.
        let held = unsafe { &*seen };
        if held.lock().is_ok_and(|kept| wanted(&kept)) {
            return Ok(());
        }
        std::thread::sleep(LOOK);
    }
    Err(format!("{what} never happened").into())
}

/// # Panics
///
/// When a command whose link went before its answer is not named in
/// `command_id`, or keystrokes dropped for want of a link are not named by
/// their pane in `pane`.
#[test]
fn lost_link_names_the_command_and_the_pane() {
    let case = || -> Result<(), Failed> {
        let held = scratch("names")?;
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        let socket = held.path.join("scripted.sock");
        scripted(&runtime, &socket)?;
        let directory = CString::new(held.path.join("runtime").display().to_string())?;
        let configuration = Configuration {
            runtime_directory: directory.as_ptr(),
            artifacts_directory: core::ptr::null(),
            askpass_program: core::ptr::null(),
            log_path: core::ptr::null(),
        };
        let mut error = blank();
        // SAFETY: the configuration is alive for this call; the error is ours.
        let made: *mut Client =
            unsafe { iznik_client_new(&raw const configuration, &raw mut error) };
        if made.is_null() {
            return Err("no client".into());
        }
        let seen: *mut Mutex<Vec<Told>> = Box::into_raw(Box::new(Mutex::new(Vec::new())));
        // SAFETY: the client is live and the context outlives it.
        unsafe { iznik_set_event_callback(made, Some(record), seen.cast::<c_void>()) };
        let alias = CString::new(format!("unix:{}", socket.display()))?;
        // SAFETY: the client is live, the alias null-terminated.
        let added = unsafe { iznik_host_add(made, alias.as_ptr(), &raw mut error) };
        assert_eq!(added, OK, "the host is taken");
        await_told(seen, "the host's model", |kept| {
            kept.iter().any(|told| told.kind == EventKind::Snapshot)
        })?;
        let asked = encode_session_command(&SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        })?;
        let mut number: u64 = 0;
        // SAFETY: every pointer is alive for the call.
        let sent = unsafe {
            iznik_command(
                made,
                alias.as_ptr(),
                asked.as_ptr(),
                asked.len(),
                &raw mut number,
                &raw mut error,
            )
        };
        assert_eq!(sent, OK, "the command goes");
        await_told(seen, "the unknown outcome", |kept| {
            kept.iter().any(|told| told.command == number)
        })?;
        let typed = b"ls\r";
        // SAFETY: every pointer is alive for the call.
        let given = unsafe {
            iznik_pane_input(
                made,
                alias.as_ptr(),
                PANE,
                typed.as_ptr(),
                typed.len(),
                &raw mut error,
            )
        };
        assert_eq!(given, OK, "the keystrokes are taken");
        await_told(seen, "the dropped keystrokes", |kept| {
            kept.iter().any(|told| told.pane == PANE)
        })?;
        // SAFETY: it came from `iznik_client_new` and is freed once, here.
        unsafe { iznik_client_free(made) };
        // SAFETY: the box this case made, taken back exactly once.
        drop(unsafe { Box::from_raw(seen) });
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
