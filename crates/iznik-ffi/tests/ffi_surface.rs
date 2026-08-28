//! The C ABI, called the way a C program calls it.
//!
//! Every case here goes through the `extern "C"` functions and nothing else:
//! no Rust type of the engine's crosses the boundary, and what is asserted is
//! what an application would see. The host is a `unix:` alias to a daemon on
//! this machine, because what is being proven is the boundary and not the
//! network — the network is `end-to-end-ssh`'s.

use core::ffi::c_void;
use core::time::Duration;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use iznik::error::{Error, INVALID_ARGUMENT, Layer, OK};
use iznik::model::{Event, EventKind};
use iznik::{
    Client, Configuration, iznik_client_free, iznik_client_new, iznik_command, iznik_host_add,
    iznik_host_uninstall, iznik_set_event_callback,
};
use iznik_protocol::command::{SessionCommand, encode_session_command};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long ending a client with hosts on it may take.
const TEARDOWN_CEILING: Duration = Duration::from_secs(1);

/// The width a pane is made at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// What the callback has been told, in the order it was told.
#[derive(Debug, Default)]
struct Seen {
    /// Every thread a callback has arrived on.
    threads: BTreeSet<String>,
    /// What each event was: its kind, its host, its payload copied while it
    /// was valid, and the command number it carried.
    events: Vec<(EventKind, String, Vec<u8>, u64)>,
}

/// The callback every case uses: it copies what it is given, because that is
/// the rule the boundary states.
extern "C" fn record(event: *const Event, context: *mut c_void) {
    if event.is_null() || context.is_null() {
        return;
    }
    // SAFETY: iznik hands a live event, valid for the length of this call,
    // which is where it is read.
    let held = unsafe { &*event };
    // SAFETY: the context is the pointer this case passed to
    // `iznik_set_event_callback`, and it outlives every callback because the
    // client is freed before it is.
    let seen = unsafe { &*context.cast::<Mutex<Seen>>() };
    let host = named(held.host);
    let payload = copied(held.payload, held.payload_length);
    let Ok(mut kept) = seen.lock() else {
        return;
    };
    kept.threads
        .insert(format!("{:?}", std::thread::current().id()));
    kept.events
        .push((held.kind, host, payload, held.command_id));
}

/// One of iznik's strings as text.
fn named(pointer: *const core::ffi::c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: iznik's own promise: null-terminated and valid for this call.
    unsafe { core::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

/// One of iznik's buffers, copied while it is valid.
fn copied(payload: *const u8, length: usize) -> Vec<u8> {
    if payload.is_null() {
        return Vec::new();
    }
    // SAFETY: iznik's own promise: `length` readable bytes, valid for this
    // call, which is where they are copied.
    unsafe { core::slice::from_raw_parts(payload, length) }.to_vec()
}

/// A temporary directory of this case's own, removed when the guard drops.
#[derive(Debug)]
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
    let path = base.join(format!("iznik-ffi-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the daemon a case stands up.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// An error struct to be filled in.
fn blank() -> Error {
    Error {
        code: OK,
        layer: Layer::Client,
        message: core::ptr::null(),
    }
}

/// What an error says.
fn said(error: &Error) -> String {
    named(error.message)
}

/// A client whose runtime files go under `held`.
///
/// # Errors
///
/// When it cannot be made, which the error says.
fn client(held: &Scratch) -> Result<*mut Client, Failed> {
    let runtime = CString::new(held.path.join("runtime").display().to_string())?;
    let configuration = Configuration {
        runtime_directory: runtime.as_ptr(),
        artifacts_directory: core::ptr::null(),
        askpass_program: core::ptr::null(),
        log_path: core::ptr::null(),
    };
    let mut error = blank();
    // SAFETY: the configuration is alive for this call and its one string is
    // null-terminated; the error is this case's own.
    let made = unsafe { iznik_client_new(&raw const configuration, &raw mut error) };
    if made.is_null() {
        return Err(format!("no client: {}", said(&error)).into());
    }
    Ok(made)
}

/// Waits until what has been seen satisfies `wanted`.
///
/// # Errors
///
/// When it never does.
fn await_seen(
    seen: *mut Mutex<Seen>,
    what: &str,
    wanted: impl Fn(&Seen) -> bool,
) -> Result<(), Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        // SAFETY: the pointer is this case's own box, alive until the case
        // ends, and every reader of it locks.
        let held = unsafe { &*seen };
        if held.lock().is_ok_and(|kept| wanted(&kept)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("{what} never happened").into())
}

/// Whether an event of that kind is among those seen.
///
/// Takes what has been seen rather than looking it up: the wait below holds
/// the lock while it asks, and a second lock on the same mutex from inside the
/// question would wait for the answer.
fn saw(kept: &Seen, kind: EventKind) -> bool {
    kept.events
        .iter()
        .any(|(named, _host, _payload, _id)| *named == kind)
}

/// # Panics
///
/// When a client cannot be made with the defaults, or one made with a runtime
/// directory it cannot use does not say so.
#[test]
fn ffi_surface_makes_and_ends_a_client() {
    let case = || -> Result<(), Failed> {
        let held = scratch("lifecycle")?;
        let mut error = blank();
        // SAFETY: a null configuration is one of the two this takes, and the
        // error is this case's own.
        let defaulted = unsafe { iznik_client_new(core::ptr::null(), &raw mut error) };
        assert!(
            !defaulted.is_null(),
            "the defaults make a client: {}",
            said(&error)
        );
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(defaulted) };
        std::fs::write(held.path.join("file"), b"not a directory")?;
        let impossible = CString::new(held.path.join("file/under/a/file").display().to_string())?;
        let configuration = Configuration {
            runtime_directory: impossible.as_ptr(),
            artifacts_directory: core::ptr::null(),
            askpass_program: core::ptr::null(),
            log_path: core::ptr::null(),
        };
        // SAFETY: the configuration is alive for this call and its string is
        // null-terminated.
        let refused = unsafe { iznik_client_new(&raw const configuration, &raw mut error) };
        assert!(
            refused.is_null(),
            "a runtime directory it cannot use is refused"
        );
        assert_eq!(error.layer, Layer::Client, "by this layer");
        assert!(!said(&error).is_empty(), "with something to read");
        // SAFETY: null is one of the two this takes.
        unsafe { iznik_client_free(core::ptr::null_mut()) };
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When adding a host does not produce the events the boundary promises, or a
/// command's answer does not come back with the number it was given.
#[test]
fn ffi_surface_carries_events_and_commands() {
    let case = || -> Result<(), Failed> {
        let held = scratch("events")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let made = client(&held)?;
        let seen: *mut Mutex<Seen> = Box::into_raw(Box::new(Mutex::new(Seen::default())));
        // SAFETY: the client is live and the context outlives it.
        unsafe { iznik_set_event_callback(made, Some(record), seen.cast::<c_void>()) };
        let alias = CString::new(format!("unix:{}", stack.socket().display()))?;
        let mut error = blank();
        // SAFETY: the client is live, the alias null-terminated, the error
        // this case's own.
        let added = unsafe { iznik_host_add(made, alias.as_ptr(), &raw mut error) };
        assert_eq!(added, OK, "the host is taken: {}", said(&error));
        await_seen(seen, "a host state", |kept| saw(kept, EventKind::HostState))?;
        await_seen(seen, "a snapshot", |kept| saw(kept, EventKind::Snapshot))?;
        {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*seen }.lock().map_err(|_broken| "the record")?;
            let snapshot = kept
                .events
                .iter()
                .find(|(kind, _host, _payload, _id)| *kind == EventKind::Snapshot)
                .ok_or("a snapshot was seen")?;
            // The payload is the protocol's own encoding, read with the
            // protocol's own reader: one schema, not two.
            let model = iznik_protocol::model::decode_host_model(&snapshot.2)?;
            assert_eq!(model.sessions.len(), 0, "a fresh daemon holds nothing");
            assert!(snapshot.1.contains("unix:"), "and it names the host");
        }
        let asked = encode_session_command(&SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        })?;
        let mut number: u64 = 0;
        // SAFETY: every pointer is alive for the call, and the bytes are this
        // case's own.
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
        assert_eq!(sent, OK, "the command goes: {}", said(&error));
        assert!(number > 0, "and comes back with a number of its own");
        // The buffer may go as soon as the call returned, which is the rule
        // the boundary states.
        drop(asked);
        await_seen(seen, "the command's answer", |kept| {
            kept.events.iter().any(|(kind, _host, _payload, id)| {
                *kind == EventKind::CommandResult && *id == number
            })
        })?;
        await_seen(seen, "the session's delta", |kept| {
            saw(kept, EventKind::Delta)
        })?;
        // SAFETY: it came from `iznik_client_new` and is freed once, here.
        unsafe { iznik_client_free(made) };
        // Nothing can be calling back now, so the record may go.
        // SAFETY: the box this case made, taken back exactly once.
        drop(unsafe { Box::from_raw(seen) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When callbacks arrive on more than one thread, or ending a client with a
/// host on it takes longer than a person would wait.
#[test]
fn ffi_surface_calls_back_on_one_thread_and_ends_quickly() {
    let case = || -> Result<(), Failed> {
        let held = scratch("threading")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let made = client(&held)?;
        let seen: *mut Mutex<Seen> = Box::into_raw(Box::new(Mutex::new(Seen::default())));
        // SAFETY: the client is live and the context outlives it.
        unsafe { iznik_set_event_callback(made, Some(record), seen.cast::<c_void>()) };
        let alias = CString::new(format!("unix:{}", stack.socket().display()))?;
        let mut error = blank();
        // SAFETY: as above.
        let _added = unsafe { iznik_host_add(made, alias.as_ptr(), &raw mut error) };
        await_seen(seen, "a snapshot", |kept| saw(kept, EventKind::Snapshot))?;
        {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*seen }.lock().map_err(|_broken| "the record")?;
            assert_eq!(
                kept.threads.len(),
                1,
                "every callback arrives on one thread: {:?}",
                kept.threads
            );
        }
        let started = Instant::now();
        // SAFETY: it came from `iznik_client_new` and is freed once, here.
        unsafe { iznik_client_free(made) };
        let taken = started.elapsed();
        assert!(
            taken < TEARDOWN_CEILING,
            "a client with a host on it ends in {taken:?}"
        );
        // SAFETY: the box this case made, taken back exactly once, after the
        // client that could have called into it is gone.
        drop(unsafe { Box::from_raw(seen) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a call with nothing to work on does not say what was wrong, or a host
/// that cannot be reached is not reported against the layer that could not
/// reach it.
#[test]
fn ffi_surface_says_which_layer_failed() {
    let case = || -> Result<(), Failed> {
        let held = scratch("errors")?;
        let made = client(&held)?;
        let mut error = blank();
        // SAFETY: the client is live; a null alias is one of the two this
        // takes.
        let refused = unsafe { iznik_host_add(made, core::ptr::null(), &raw mut error) };
        assert_eq!(
            refused, INVALID_ARGUMENT,
            "a call with no host named refuses"
        );
        assert_eq!(error.layer, Layer::Client, "against this layer");
        assert!(!said(&error).is_empty(), "with something to read");
        // A socket that is not there: taken as a host, because a host must be
        // held before it can be asked to give anything up.
        let nowhere = CString::new(format!("unix:{}", held.path.join("nowhere.sock").display()))?;
        // SAFETY: the client is live and the alias null-terminated.
        let taken = unsafe { iznik_host_add(made, nowhere.as_ptr(), &raw mut error) };
        assert_eq!(taken, OK, "a host that is not there is still held");
        // SAFETY: the client is live and the alias null-terminated.
        let unreachable = unsafe { iznik_host_uninstall(made, nowhere.as_ptr(), &raw mut error) };
        assert_ne!(unreachable, OK, "an unreachable host is a failure");
        assert_eq!(
            error.layer,
            Layer::Transport,
            "reported against the layer that could not reach it: {}",
            said(&error)
        );
        assert!(
            said(&error).contains("nowhere.sock"),
            "and it names the host: {}",
            said(&error)
        );
        // SAFETY: it came from `iznik_client_new` and is freed once, here.
        unsafe { iznik_client_free(made) };
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
