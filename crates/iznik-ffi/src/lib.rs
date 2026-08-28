//! The C ABI over `iznik-client`: the entry points the application calls, the error, the events, and the pane byte pipe.
#![doc = include_str!("../README.md")]

pub mod error;
pub mod model;
pub mod pane;

use core::ffi::{c_char, c_int, c_void};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, ThreadId};

use iznik_client::bootstrap::launch::{Stage, UpgradeError};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{HostManager, ManagerError, ManagerEvent, ManagerOptions};
use iznik_client::reduce::Notification;
use iznik_client::transport::ClientRuntimePaths;
use iznik_client::transport::ssh::SshOptions;
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::{ToClient, encode_to_client};

use crate::error::{Error, INVALID_ARGUMENT, Layer, OK, REFUSED, UNKNOWN_HOST};
use crate::model::{Event, EventCallback, EventKind};
use crate::pane::PaneCallbacks;

/// The directory artifacts are looked for in when the application names none.
const ARTIFACTS_DIRECTORY: &str = "artifacts";

/// What every call answers when it did what it was asked.
const DONE: c_int = OK;

/// A pointer the application gave and iznik carries back to it untouched.
///
/// The application decides what it means; this only has to hold it and hand
/// it to the one thread that calls the callback.
#[derive(Clone, Copy, Debug)]
struct Carried(*mut c_void);

// SAFETY: what is carried is opaque here — nothing in this library reads or
// writes through it. The application's obligation, stated in the header, is
// that whatever it points at may be used from the callback thread; moving the
// pointer to that thread is what this promises and all it promises.
unsafe impl Send for Carried {}

/// What the application asked to be told, and what to tell it with.
#[derive(Debug)]
struct Listener {
    /// What to call.
    callback: EventCallback,
    /// What to pass it.
    context: Carried,
}

/// The client the application holds a pointer to.
///
/// Opaque across the boundary: everything about it is behind the functions
/// below, and its size is nobody else's business.
#[derive(Debug)]
pub struct Client {
    /// The engine itself. In an `Option` so that ending it, and with it the
    /// stream of events, can be done before the thread reading them is waited
    /// for.
    manager: Option<HostManager>,
    /// Serializes the application's own calls, and is never held while a
    /// callback runs.
    calling: Mutex<()>,
    /// What to tell, and what to tell it with.
    listening: Arc<Mutex<Listener>>,
    /// The panes an application has attached to, and what to call for each.
    attached: Arc<Mutex<BTreeMap<(HostId, PaneId), Attached>>>,
    /// Held by the thread below for as long as a callback is running.
    ///
    /// Not taken in order to make a call — taken to say that one is being
    /// made, so that letting a pane go, or taking the event callback away,
    /// can wait for the call that may already be running instead of leaving
    /// an application to guess whether its context is still being read.
    delivering: Arc<Mutex<()>>,
    /// Which thread that is, so that a handler calling back in is never made
    /// to wait for itself.
    delivers: ThreadId,
    /// The one thread every callback arrives on.
    pump: Option<JoinHandle<()>>,
}

/// One pane an application is watching.
#[derive(Clone, Copy, Debug)]
struct Attached {
    /// What to call.
    callbacks: PaneCallbacks,
    /// What to pass it.
    context: Carried,
}

impl Client {
    /// Waits for a callback that may already be running to finish.
    ///
    /// Taken after whatever is being taken away is out of reach and never
    /// before: a call that has not begun will not find it, and one that has
    /// is what this waits for. So an application that has detached a pane, or
    /// replaced its event callback, may free what it passed as soon as the
    /// call it made returns.
    ///
    /// Nothing else may be held while this waits — the thread it waits for
    /// makes calls that come back in and take those locks. A handler that
    /// calls in from inside a callback waits for nothing at all: it *is* the
    /// call, and what it passed is alive for as long as it is running.
    fn quiesce(&self) {
        if std::thread::current().id() == self.delivers {
            return;
        }
        drop(self.delivering.lock());
    }

    /// Takes the lock that serializes the application's own calls.
    ///
    /// It is never held while a callback runs, so a handler may call back in.
    fn serialize(&self) -> Option<std::sync::MutexGuard<'_, ()>> {
        self.calling.lock().ok()
    }

    /// The engine, while there is one.
    fn manager(&self) -> Option<&HostManager> {
        self.manager.as_ref()
    }

    /// Begins watching a pane.
    fn attach(&self, host: &str, pane: PaneId, callbacks: PaneCallbacks, context: *mut c_void) {
        if let Ok(mut held) = self.attached.lock() {
            let _before = held.insert(
                (HostId(host.to_owned()), pane),
                Attached {
                    callbacks,
                    context: Carried(context),
                },
            );
        }
    }

    /// Stops watching one.
    fn forget(&self, host: &str, pane: PaneId) {
        if let Ok(mut held) = self.attached.lock() {
            let _gone = held.remove(&(HostId(host.to_owned()), pane));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The manager first, and dropped rather than merely taken: closing the
        // stream the thread below is reading is what lets that thread end, and
        // a binding that held it to the end of this block would have the join
        // wait for a thread waiting for it.
        drop(self.manager.take());
        if let Some(pump) = self.pump.take() {
            let _joined = pump.join();
        }
    }
}

/// What the application makes a client with.
///
/// Every field may be null, and null means the default.
#[repr(C)]
#[derive(Debug)]
pub struct Configuration {
    /// Where this machine's own runtime files go.
    pub runtime_directory: *const c_char,
    /// Where the servers this build carries are.
    pub artifacts_directory: *const c_char,
    /// The program `ssh` asks for a passphrase with.
    pub askpass_program: *const c_char,
    /// Where to write a log.
    pub log_path: *const c_char,
}

/// One C string as text, when it is one.
///
/// # Safety
///
/// `pointer` is null, or points at a null-terminated string that stays valid
/// for the length of this call.
unsafe fn text<'held>(pointer: *const c_char) -> Option<&'held str> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation, above: not null, null-terminated, and
    // alive for this call, which is exactly what `from_ptr` asks for.
    let held = unsafe { CStr::from_ptr(pointer) };
    held.to_str().ok()
}

/// The client a pointer names, when it names one.
///
/// # Safety
///
/// `client` is null, or a pointer [`iznik_client_new`] gave back and
/// [`iznik_client_free`] has not taken.
unsafe fn borrowed<'held>(client: *mut Client) -> Option<&'held Client> {
    if client.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation, above: the pointer came from a
    // `Box::into_raw` in `iznik_client_new` and has not been freed, so it
    // points at a live `Client` this may borrow for the call.
    Some(unsafe { &*client })
}

/// The options a configuration says, filled in with the defaults it leaves
/// out.
///
/// # Errors
///
/// The words for a person when the runtime paths cannot be made.
fn options(
    runtime: Option<&str>,
    artifacts: Option<&str>,
    askpass: Option<&str>,
    log: Option<&str>,
) -> Result<ManagerOptions, String> {
    let paths = match runtime {
        Some(named) => ClientRuntimePaths::under(&PathBuf::from(named)),
        None => ClientRuntimePaths::resolve(),
    }
    .map_err(|error| error.to_string())?;
    let carried = match artifacts {
        Some(named) => PathBuf::from(named),
        None => paths.directory.join(ARTIFACTS_DIRECTORY),
    };
    // A place of iznik's own, made rather than demanded: an application that
    // ships no artifacts still reaches a `unix:` daemon, and one that ships
    // them names the directory it put them in.
    std::fs::create_dir_all(&carried).map_err(|error| format!("{}: {error}", carried.display()))?;
    let mut held = ManagerOptions::new(carried, paths);
    held.ssh = SshOptions {
        askpass_program: askpass.map(PathBuf::from),
        ..SshOptions::default()
    };
    held.log_path = log.map(PathBuf::from);
    Ok(held)
}

/// Makes a client.
///
/// Answers null when it cannot, having filled in `error`.
///
/// **Obligation:** the pointer this gives back is freed exactly once, with
/// [`iznik_client_free`], and not used afterwards. Everything the
/// configuration points at may be freed as soon as this returns.
///
/// # Safety
///
/// `configuration` is null or points at a [`Configuration`] whose strings are
/// null or null-terminated, and `error` is null or points at an [`Error`] the
/// caller owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_client_new(
    configuration: *const Configuration,
    error: *mut Error,
) -> *mut Client {
    // SAFETY: the caller's obligation, above.
    unsafe { error::clear(error) };
    let said = if configuration.is_null() {
        None
    } else {
        // SAFETY: the caller's obligation: not null here, and it points at a
        // `Configuration` that is valid for this call.
        Some(unsafe { &*configuration })
    };
    // SAFETY: null or a null-terminated string of the caller's, which is
    // `text`'s obligation.
    let runtime = unsafe { said.and_then(|held| text(held.runtime_directory)) };
    // SAFETY: the same obligation, for the next of them.
    let artifacts = unsafe { said.and_then(|held| text(held.artifacts_directory)) };
    // SAFETY: the same again.
    let askpass = unsafe { said.and_then(|held| text(held.askpass_program)) };
    // SAFETY: and the last of them.
    let log = unsafe { said.and_then(|held| text(held.log_path)) };
    let held = match options(runtime, artifacts, askpass, log) {
        Ok(held) => held,
        Err(detail) => {
            // SAFETY: the caller's obligation, above.
            unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, &detail) };
            return core::ptr::null_mut();
        }
    };
    let manager = match HostManager::new(held) {
        Ok(manager) => manager,
        Err(refusal) => {
            // SAFETY: the caller's obligation, above.
            unsafe { error::fill(error, REFUSED, layer_of(&refusal), &refusal.to_string()) };
            return core::ptr::null_mut();
        }
    };
    Box::into_raw(Box::new(started(manager)))
}

/// A client around a manager, with the one thread its callbacks arrive on.
fn started(manager: HostManager) -> Client {
    let events = manager.events();
    let listening = Arc::new(Mutex::new(Listener {
        callback: None,
        context: Carried(core::ptr::null_mut()),
    }));
    let attached = Arc::new(Mutex::new(BTreeMap::new()));
    let telling = Arc::clone(&listening);
    let watching = Arc::clone(&attached);
    let delivering = Arc::new(Mutex::new(()));
    let busy = Arc::clone(&delivering);
    let pump = std::thread::spawn(move || deliver(&telling, &watching, &busy, &events));
    let delivers = pump.thread().id();
    Client {
        manager: Some(manager),
        calling: Mutex::new(()),
        listening,
        attached,
        delivering,
        delivers,
        pump: Some(pump),
    }
}

/// Reads every event and hands it to the application, on this one thread.
///
/// The lock over the listener is taken to read it and let go before the call,
/// so an application that calls back into iznik from its handler waits for
/// nothing this thread holds.
fn deliver(
    listening: &Arc<Mutex<Listener>>,
    attached: &Arc<Mutex<BTreeMap<(HostId, PaneId), Attached>>>,
    delivering: &Arc<Mutex<()>>,
    events: &Receiver<ManagerEvent>,
) {
    while let Ok(event) = events.recv() {
        // Taken for the whole of the call and let go between calls: what
        // waits on it is a caller taking away the context this one is about
        // to read.
        let Ok(_delivering) = delivering.lock() else {
            return;
        };
        // A pane somebody is watching is handed its own bytes, and not handed
        // them twice: an application that attached to a pane reads it there.
        if handed(attached, &event) {
            continue;
        }
        let Ok(held) = listening.lock() else {
            return;
        };
        let (callback, context) = (held.callback, held.context);
        drop(held);
        let Some(callback) = callback else {
            continue;
        };
        carry(callback, context, &event);
    }
}

/// Hands one event to the pane it is about, when somebody is watching that
/// pane, and says whether it did.
///
/// The lock is let go before the call, for the reason every other callback is
/// made without one: the handler may call straight back in.
fn handed(
    attached: &Arc<Mutex<BTreeMap<(HostId, PaneId), Attached>>>,
    event: &ManagerEvent,
) -> bool {
    let Some((host, pane)) = about(event) else {
        return false;
    };
    let Ok(held) = attached.lock() else {
        return false;
    };
    let Some(watching) = held.get(&(host, pane)).copied() else {
        return false;
    };
    drop(held);
    to_the_pane(&watching, event)
}

/// Which host and pane an event is about, when it is about one.
fn about(event: &ManagerEvent) -> Option<(HostId, PaneId)> {
    match event {
        ManagerEvent::Bytes { host, pane, .. }
        | ManagerEvent::Screen { host, pane, .. }
        | ManagerEvent::Detached { host, pane }
        | ManagerEvent::Notify(Notification::Mark { host, pane, .. }) => {
            Some((host.clone(), *pane))
        }
        _elsewhere => None,
    }
}

/// Hands one event to a pane's own handlers, and says whether one took it.
fn to_the_pane(watching: &Attached, event: &ManagerEvent) -> bool {
    let context = watching.context.0;
    match event {
        ManagerEvent::Bytes { bytes, .. } => match watching.callbacks.output {
            Some(output) => {
                output(context, bytes.as_ptr(), bytes.len());
                true
            }
            None => false,
        },
        ManagerEvent::Screen {
            sequence,
            columns,
            rows,
            bytes,
            ..
        } => match watching.callbacks.screen {
            Some(screen) => {
                screen(
                    context,
                    sequence.0,
                    *columns,
                    *rows,
                    bytes.as_ptr(),
                    bytes.len(),
                );
                true
            }
            None => false,
        },
        ManagerEvent::Detached { .. } => match watching.callbacks.detached {
            Some(detached) => {
                detached(context);
                true
            }
            None => false,
        },
        ManagerEvent::Notify(notification) => marked(watching, notification, context),
        _elsewhere => false,
    }
}

/// Hands a shell-integration event to a pane's own handler.
fn marked(watching: &Attached, notification: &Notification, context: *mut c_void) -> bool {
    let Notification::Mark {
        pane,
        sequence,
        kind,
        ..
    } = notification
    else {
        return false;
    };
    let Some(mark) = watching.callbacks.mark else {
        return false;
    };
    let Ok(payload) = encode_to_client(&ToClient::Mark {
        pane: *pane,
        sequence: *sequence,
        kind: kind.clone(),
    }) else {
        return false;
    };
    mark(context, sequence.0, payload.as_ptr(), payload.len());
    true
}

/// Hands one event over, with every pointer in it alive for exactly the call.
fn carry(
    callback: extern "C" fn(*const Event, *mut c_void),
    context: Carried,
    event: &ManagerEvent,
) {
    let Some((kind, host, payload)) = shaped(event) else {
        return;
    };
    let Ok(named) = CString::new(host) else {
        return;
    };
    let held = Event {
        kind,
        host: named.as_ptr(),
        pane: pane_of(event),
        sequence: sequence_of(event),
        generation: generation_of(event),
        command_id: command_of(event),
        payload: payload.as_ptr(),
        payload_length: payload.len(),
    };
    callback(&raw const held, context.0);
}

/// What kind an event is, whose host it is about, and the bytes it carries.
fn shaped(event: &ManagerEvent) -> Option<(EventKind, String, Vec<u8>)> {
    match event {
        ManagerEvent::Moved { host, state } => Some((
            EventKind::HostState,
            host.0.clone(),
            state.to_string().into_bytes(),
        )),
        ManagerEvent::Snapshot { host, payload, .. } => {
            Some((EventKind::Snapshot, host.0.clone(), payload.clone()))
        }
        ManagerEvent::Delta { host, payload, .. } => {
            Some((EventKind::Delta, host.0.clone(), payload.clone()))
        }
        ManagerEvent::Bytes { host, bytes, .. } => {
            Some((EventKind::PaneBytes, host.0.clone(), bytes.clone()))
        }
        ManagerEvent::Screen { host, bytes, .. } => {
            Some((EventKind::Screen, host.0.clone(), bytes.clone()))
        }
        // A pane nobody attached to has stopped: the state it belongs to is
        // what an application watching from a distance reads.
        ManagerEvent::Detached { host, pane } => Some((
            EventKind::HostState,
            host.0.clone(),
            format!("pane {} detached", pane.0).into_bytes(),
        )),
        ManagerEvent::Notify(notification) => told(notification),
        // A host nobody holds any more is not an event with a payload; the
        // application learns it from the state that came before it.
        ManagerEvent::Removed { host } => Some((
            EventKind::HostState,
            host.0.clone(),
            "removed".to_owned().into_bytes(),
        )),
    }
}

/// One notification as a kind and a payload.
fn told(notification: &Notification) -> Option<(EventKind, String, Vec<u8>)> {
    match notification {
        Notification::CommandFinished { host, outcome, .. } => {
            let payload = iznik_protocol::command::encode_command_outcome(outcome).ok()?;
            Some((EventKind::CommandResult, host.0.clone(), payload))
        }
        Notification::Mark {
            host,
            pane,
            sequence,
            kind,
        } => {
            let payload = encode_to_client(&ToClient::Mark {
                pane: *pane,
                sequence: *sequence,
                kind: kind.clone(),
            })
            .ok()?;
            Some((EventKind::Mark, host.0.clone(), payload))
        }
        Notification::CommandTimedOut { host, command } => Some((
            EventKind::Notification,
            host.0.clone(),
            format!("command {} was never answered", command.0).into_bytes(),
        )),
        Notification::Refused {
            host,
            code,
            message,
        } => Some((
            EventKind::Notification,
            host.0.clone(),
            format!("{code:?}: {message}").into_bytes(),
        )),
        Notification::Malformed { host, detail } => Some((
            EventKind::Notification,
            host.0.clone(),
            detail.clone().into_bytes(),
        )),
    }
}

/// The pane an event is about, or zero.
fn pane_of(event: &ManagerEvent) -> u64 {
    match event {
        ManagerEvent::Bytes { pane, .. }
        | ManagerEvent::Screen { pane, .. }
        | ManagerEvent::Notify(Notification::Mark { pane, .. }) => pane.0,
        _elsewhere => 0,
    }
}

/// The number a model event carries, or zero.
///
/// A change is applied to the generation before it and to no other, so an
/// application that keeps a model of its own cannot use one without the
/// number it belongs to: `iznik_protocol`'s own `apply` takes both, and the
/// encoded change does not carry it.
fn generation_of(event: &ManagerEvent) -> u64 {
    match event {
        ManagerEvent::Snapshot { generation, .. } | ManagerEvent::Delta { generation, .. } => {
            generation.0
        }
        _elsewhere => 0,
    }
}

/// Where in a pane's stream an event sits, or zero.
fn sequence_of(event: &ManagerEvent) -> u64 {
    match event {
        ManagerEvent::Bytes { sequence, .. }
        | ManagerEvent::Screen { sequence, .. }
        | ManagerEvent::Notify(Notification::Mark { sequence, .. }) => sequence.0,
        _elsewhere => 0,
    }
}

/// This client's number for the command an event is about, or zero.
fn command_of(event: &ManagerEvent) -> u64 {
    match event {
        ManagerEvent::Notify(
            Notification::CommandFinished { command, .. }
            | Notification::CommandTimedOut { command, .. },
        ) => command.0,
        _elsewhere => 0,
    }
}

/// Which layer a refusal came from.
///
/// The first question anybody asks, so it is answered as precisely as what is
/// known allows: a bootstrap carries the stage it stopped at, and the stage
/// says which layer that was.
fn layer_of(refusal: &ManagerError) -> Layer {
    match refusal {
        ManagerError::Upgrade { source } => upgrading(source),
        ManagerError::Uninstall { source } => staged(source.stage),
        ManagerError::Runtime { .. }
        | ManagerError::Artifacts { .. }
        | ManagerError::Log { .. }
        | ManagerError::UnknownHost { .. }
        | ManagerError::Gone { .. }
        | ManagerError::Poisoned { .. } => Layer::Client,
    }
}

/// Which layer an upgrade's refusal came from.
fn upgrading(refusal: &UpgradeError) -> Layer {
    match refusal {
        // The host is holding panes: that is the server's own answer about
        // its own state, not a failure of anything below it.
        UpgradeError::LivePanes { .. } => Layer::Server,
        UpgradeError::Bootstrap(source) => staged(source.stage),
    }
}

/// Which layer a bootstrap's stage belongs to.
fn staged(stage: Stage) -> Layer {
    match stage {
        // The probe is one round trip and nothing else: when it fails, what
        // failed is the way to the host.
        Stage::Probe => Layer::Transport,
        Stage::Upload | Stage::Launch => Layer::Bootstrap,
        Stage::Handshake => Layer::Protocol,
    }
}

/// Ends a client: every host let go, every task stopped, the callback thread
/// joined.
///
/// **Obligation:** the pointer came from [`iznik_client_new`], has not been
/// freed, and is not used afterwards. A null pointer is nothing to free and
/// is ignored.
///
/// # Safety
///
/// As the obligation above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_client_free(client: *mut Client) {
    if client.is_null() {
        return;
    }
    // SAFETY: the caller's obligation, above: the pointer came from
    // `Box::into_raw` in `iznik_client_new` and has not been freed, so taking
    // it back into a `Box` is taking back what that `Box` owned.
    let held = unsafe { Box::from_raw(client) };
    drop(held);
}

/// Says what to call when something happens, and what to pass it.
///
/// Every call arrives on one thread, and iznik holds no lock of its own while
/// one runs — a handler may call straight back in.
///
/// **Obligation:** whatever `context` points at outlives the client, or the
/// callback is set to null before it goes away.
///
/// # Safety
///
/// `client` is a live client from [`iznik_client_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_set_event_callback(
    client: *mut Client,
    callback: EventCallback,
    context: *mut c_void,
) {
    // SAFETY: the caller's obligation, above.
    let Some(held) = (unsafe { borrowed(client) }) else {
        return;
    };
    {
        let Ok(mut listening) = held.listening.lock() else {
            return;
        };
        listening.callback = callback;
        listening.context = Carried(context);
    }
    // The listener is let go first: the thread this waits for takes it to
    // read what to call.
    held.quiesce();
}

/// Begins holding a host, and connecting to it.
///
/// Returns at once; what happens next arrives on the callback.
///
/// **Obligation:** `alias` is a null-terminated UTF-8 string, and may be freed
/// as soon as this returns.
///
/// # Safety
///
/// `client` is a live client, `alias` a null-terminated string, and `error`
/// null or an [`Error`] the caller owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_host_add(
    client: *mut Client,
    alias: *const c_char,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, above, for each of the three.
    unsafe {
        with_host(client, alias, error, |manager, named| {
            manager.add_host(named);
            Ok(())
        })
    }
}

/// Stops holding a host.
///
/// # Safety
///
/// As [`iznik_host_add`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_host_remove(
    client: *mut Client,
    alias: *const c_char,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_host_add`'s.
    unsafe { with_host(client, alias, error, HostManager::remove_host) }
}

/// Drops a host's link and opens another at once.
///
/// # Safety
///
/// As [`iznik_host_add`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_host_reconnect(
    client: *mut Client,
    alias: *const c_char,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_host_add`'s.
    unsafe { with_host(client, alias, error, HostManager::reconnect) }
}

/// Replaces the server on a host with the one this build carries.
///
/// Refuses while the host holds panes unless `force` says otherwise, because
/// replacing the daemon ends them.
///
/// # Safety
///
/// As [`iznik_host_add`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_host_upgrade(
    client: *mut Client,
    alias: *const c_char,
    force: bool,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_host_add`'s.
    unsafe {
        with_host(client, alias, error, |manager, named| {
            manager.upgrade(named, force)
        })
    }
}

/// Takes iznik off a host, and stops holding it.
///
/// # Safety
///
/// As [`iznik_host_add`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_host_uninstall(
    client: *mut Client,
    alias: *const c_char,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_host_add`'s.
    unsafe { with_host(client, alias, error, HostManager::uninstall) }
}

/// Sends a session command, encoded as the protocol encodes it, and gives back
/// the number its answer will carry.
///
/// **Obligation:** the bytes are the application's and may be freed as soon as
/// this returns; iznik reads them before it does.
///
/// # Safety
///
/// `client` is a live client, `host` a null-terminated string, `command`
/// points at `length` readable bytes, and `out_command_id` and `error` are
/// null or point at storage the caller owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_command(
    client: *mut Client,
    host: *const c_char,
    command: *const u8,
    length: usize,
    out_command_id: *mut u64,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligation, above.
    unsafe { error::clear(error) };
    // SAFETY: the caller's obligation, above.
    let Some(held) = (unsafe { borrowed(client) }) else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, "no client") };
        return INVALID_ARGUMENT;
    };
    // SAFETY: the caller's obligation, above.
    let Some(named) = (unsafe { text(host) }) else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, "no host named") };
        return INVALID_ARGUMENT;
    };
    if command.is_null() {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, "no command given") };
        return INVALID_ARGUMENT;
    }
    // SAFETY: the caller's obligation: `command` points at `length` readable
    // bytes for the length of this call, which is what `from_raw_parts` asks
    // for. It is read here and nowhere else, before this returns.
    let bytes = unsafe { core::slice::from_raw_parts(command, length) };
    let asked = match iznik_protocol::command::decode_session_command(bytes) {
        Ok(asked) => asked,
        Err(refusal) => {
            // SAFETY: the caller's obligation, above.
            unsafe {
                error::fill(
                    error,
                    INVALID_ARGUMENT,
                    Layer::Protocol,
                    &format!("the command could not be read: {refusal}"),
                );
            };
            return INVALID_ARGUMENT;
        }
    };
    let Some(_serialized) = held.serialize() else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, REFUSED, Layer::Client, "the client is broken") };
        return REFUSED;
    };
    let Some(manager) = held.manager() else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, REFUSED, Layer::Client, "the client is ending") };
        return REFUSED;
    };
    match manager.command(named, asked) {
        Ok(submission) => {
            if !out_command_id.is_null() {
                // SAFETY: the caller's obligation: not null here, and it
                // points at storage the caller owns and may be written.
                unsafe { out_command_id.write(submission.id.0) };
            }
            DONE
        }
        Err(refusal) => {
            // SAFETY: the caller's obligation, above.
            unsafe {
                error::fill(
                    error,
                    code_of(&refusal),
                    layer_of(&refusal),
                    &refusal.to_string(),
                );
            };
            code_of(&refusal)
        }
    }
}

/// Runs one operation that names a host, with the calls serialized and the
/// error filled in.
///
/// # Safety
///
/// As [`iznik_host_add`].
unsafe fn with_host(
    client: *mut Client,
    alias: *const c_char,
    error: *mut Error,
    doing: impl FnOnce(&HostManager, &str) -> Result<(), ManagerError>,
) -> c_int {
    // SAFETY: the caller's obligation, above.
    unsafe { error::clear(error) };
    // SAFETY: the caller's obligation, above.
    let Some(held) = (unsafe { borrowed(client) }) else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, "no client") };
        return INVALID_ARGUMENT;
    };
    // SAFETY: the caller's obligation, above.
    let Some(named) = (unsafe { text(alias) }) else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, "no host named") };
        return INVALID_ARGUMENT;
    };
    let Some(_serialized) = held.serialize() else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, REFUSED, Layer::Client, "the client is broken") };
        return REFUSED;
    };
    let Some(manager) = held.manager() else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, REFUSED, Layer::Client, "the client is ending") };
        return REFUSED;
    };
    match doing(manager, named) {
        Ok(()) => DONE,
        Err(refusal) => {
            // SAFETY: the caller's obligation, above.
            unsafe {
                error::fill(
                    error,
                    code_of(&refusal),
                    layer_of(&refusal),
                    &refusal.to_string(),
                );
            };
            code_of(&refusal)
        }
    }
}

/// The code a refusal answers with.
fn code_of(refusal: &ManagerError) -> c_int {
    match refusal {
        ManagerError::UnknownHost { .. } => UNKNOWN_HOST,
        _otherwise => REFUSED,
    }
}
