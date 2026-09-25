//! The C ABI over `iznik-client`: the entry points the application calls, the error, the events, and the pane byte pipe.
#![doc = include_str!("../README.md")]

#[cfg(unix)]
mod adopt;
mod delivery;
pub mod error;
#[cfg(unix)]
pub use adopt::{
    AdoptFailure, adopt_file, adopt_listener, duplicate_descriptor, keep_across_exec, pty_adopt,
    termios_image,
};
pub mod model;
pub mod pane;
mod shape;
mod status;

use core::ffi::{c_char, c_int, c_void};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{JoinHandle, ThreadId};

use iznik_client::bootstrap::launch::{Stage, UpgradeError};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{HostManager, ManagerError, ManagerOptions};
use iznik_client::transport::ClientRuntimePaths;
use iznik_client::transport::ssh::SshOptions;
use iznik_protocol::identity::PaneId;

use crate::delivery::{Attached, Carried, Deliveries, Listener, deliver};
use crate::error::{Error, INVALID_ARGUMENT, Layer, OK, REFUSED, UNKNOWN_HOST};
use crate::model::EventCallback;
use crate::pane::PaneCallbacks;

/// The directory artifacts are looked for in when the application names none.
const ARTIFACTS_DIRECTORY: &str = "artifacts";

/// The version of the boundary this header describes.
///
/// It changes when anything an application built against the header relies
/// on changes — a signature, a structure, the meaning of a code or of an
/// obligation — and not when the library merely gets better. An application
/// compares it with what [`iznik_abi_version`] says the library it loaded
/// was built for, and refuses to run on a mismatch rather than calling into a
/// library that means something else by the same names.
pub const ABI_VERSION: u32 = 1;

/// This library's own version, null-terminated, as [`iznik_version`] gives it.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// What every call answers when it did what it was asked.
const DONE: c_int = OK;

/// The client the application holds a pointer to.
///
/// Opaque across the boundary: everything about it is behind the functions
/// below, and its size is nobody else's business.
#[derive(Debug)]
pub struct Client {
    /// The engine itself, shared with every call that is using it.
    ///
    /// The lock is held only to take a share of it — never across what a call
    /// does with it, because an uninstall takes minutes and every other call,
    /// the credit that keeps a pane printing among them, would wait behind
    /// it. In an `Option` so that ending it, and with it the stream of events,
    /// can be done before the thread reading them is waited for.
    manager: Mutex<Option<Arc<HostManager>>>,
    /// What to tell, and what to tell it with.
    listening: Arc<Mutex<Listener>>,
    /// The panes an application has attached to, and what to call for each.
    attached: Arc<Mutex<BTreeMap<(HostId, PaneId), Attached>>>,
    /// How many callbacks have begun and how many have returned, so that
    /// [`iznik_wait_for_callbacks`] can wait for the ones already running
    /// without anything being held while they run.
    deliveries: Arc<Deliveries>,
    /// Which thread the callbacks arrive on, so that a handler calling back
    /// in is never made to wait for itself.
    delivers: ThreadId,
    /// The one thread every callback arrives on.
    pump: Option<JoinHandle<()>>,
    /// The calls under way, and whether the client is being freed.
    calls: Calls,
}

/// How many calls are under way, and whether the client is ending.
///
/// Freeing a client waits for the calls already inside it — one a handler
/// made from the callback thread among them — and refuses any that arrive
/// once it has begun, so no call is ever left holding a share of an engine
/// that is being taken apart.
#[derive(Debug, Default)]
struct Calls {
    /// The two.
    state: Mutex<CallState>,
    /// Signalled whenever a call leaves.
    left: Condvar,
}

/// What [`Calls`] keeps.
#[derive(Debug, Default)]
struct CallState {
    /// How many calls are inside.
    inside: usize,
    /// Whether the client is being freed.
    ending: bool,
}

/// One call under way; it leaves when this is dropped.
#[derive(Debug)]
struct Call<'client> {
    /// Whose calls it is counted among.
    calls: &'client Calls,
}

impl Drop for Call<'_> {
    fn drop(&mut self) {
        if let Ok(mut held) = self.calls.state.lock() {
            held.inside = held.inside.saturating_sub(1);
        }
        self.calls.left.notify_all();
    }
}

impl Calls {
    /// Counts one call in, unless the client is ending.
    fn enter(&self) -> Option<Call<'_>> {
        let mut held = self.state.lock().ok()?;
        if held.ending {
            return None;
        }
        held.inside = held.inside.saturating_add(1);
        Some(Call { calls: self })
    }

    /// Refuses every call from now on, and waits for the ones inside to
    /// leave.
    fn close(&self) {
        let Ok(mut held) = self.state.lock() else {
            return;
        };
        held.ending = true;
        let _left = self.left.wait_while(held, |state| state.inside > 0);
    }
}

impl Client {
    /// Waits for every callback that had begun to return — unless this is
    /// the thread they run on, where the one running is the caller itself.
    fn quiesce(&self) {
        if std::thread::current().id() == self.delivers {
            return;
        }
        self.deliveries.wait();
    }

    /// Counts one call of the application's in, unless the client is ending.
    fn enter(&self) -> Option<Call<'_>> {
        self.calls.enter()
    }

    /// A share of the engine, while there is one.
    ///
    /// The lock is let go before this returns: what is done with the engine
    /// is done without it, so a slow call holds nobody else up.
    fn manager(&self) -> Option<Arc<HostManager>> {
        self.manager.lock().ok()?.clone()
    }

    /// Counts one call in and gives it a share of the engine — in that
    /// order, and the share only once the call is in.
    ///
    /// A call refused entry must touch nothing more of the client: the client
    /// may be being freed on another thread, which waits only for the calls
    /// it counted.
    fn engaged(&self) -> Option<(Call<'_>, Arc<HostManager>)> {
        let call = self.enter()?;
        let manager = self.manager()?;
        Some((call, manager))
    }

    /// Begins watching a pane, and gives back whoever was watching it.
    fn attach(
        &self,
        host: &str,
        pane: PaneId,
        callbacks: PaneCallbacks,
        context: *mut c_void,
    ) -> Option<Attached> {
        let mut held = self.attached.lock().ok()?;
        held.insert(
            (HostId(host.to_owned()), pane),
            Attached {
                callbacks,
                context: Carried(context),
            },
        )
    }

    /// Puts back whoever was watching a pane before an attachment that failed.
    ///
    /// An attachment that answers a refusal must leave nothing of itself
    /// behind: the application is about to free what it passed, having been
    /// told the call did not happen.
    fn restore(&self, host: &str, pane: PaneId, before: Option<Attached>) {
        if let Ok(mut held) = self.attached.lock() {
            let named = (HostId(host.to_owned()), pane);
            match before {
                Some(watching) => {
                    let _replaced = held.insert(named, watching);
                }
                None => {
                    let _gone = held.remove(&named);
                }
            }
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
        // No call begins from here on, and no callback: what a callback would
        // be given is taken away, and a wait for callbacks is let go, because
        // the one it would be waiting for may be the one freeing this.
        self.deliveries.close();
        if let Ok(mut listening) = self.listening.lock() {
            listening.callback = None;
        }
        if let Ok(mut attached) = self.attached.lock() {
            attached.clear();
        }
        // Every call already inside — from any thread, a handler's among them
        // — finishes with the share of the engine it took.
        self.calls.close();
        // The manager next, and dropped rather than merely taken: closing the
        // stream the thread below is reading is what lets that thread end, and
        // a binding that held it to the end of this block would have the join
        // wait for a thread waiting for it. Taken under the lock, not through
        // `get_mut`: a call that was refused entry may still be on its way
        // out on another thread, and nothing but the lock orders the two.
        let shared: &Client = self;
        let taken = shared
            .manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(taken);
        let Some(pump) = self.pump.take() else {
            return;
        };
        // Freed from inside a callback, the thread to wait for is this one:
        // it is let go instead, and ends by itself once the handler returns,
        // with nothing left to call and nothing left to read.
        if std::thread::current().id() != self.delivers {
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

/// A C string that is not UTF-8, which nothing at this boundary accepts.
#[derive(Clone, Copy, Debug)]
struct NotText;

/// One C string as text: nothing for a null, and a refusal for bytes that are
/// not UTF-8 — never a quiet nothing, because a path or an alias that is not
/// what it was given as is worse than one refused.
///
/// # Errors
///
/// [`NotText`] when the bytes are not UTF-8.
///
/// # Safety
///
/// `pointer` is null, or points at a null-terminated string that stays valid
/// for the length of this call.
unsafe fn text<'held>(pointer: *const c_char) -> Result<Option<&'held str>, NotText> {
    if pointer.is_null() {
        return Ok(None);
    }
    // SAFETY: the caller's obligation, above: not null, null-terminated, and
    // alive for this call, which is exactly what `from_ptr` asks for.
    let held = unsafe { CStr::from_ptr(pointer) };
    held.to_str().map(Some).map_err(|_not_utf8| NotText)
}

/// A host's name, or the words that say why there is none.
///
/// # Errors
///
/// Those words, for a null and for bytes that are not UTF-8.
///
/// # Safety
///
/// As [`text`].
unsafe fn host_named<'held>(pointer: *const c_char) -> Result<&'held str, &'static str> {
    // SAFETY: the caller's obligation, above, which is `text`'s.
    match unsafe { text(pointer) } {
        Ok(Some(named)) => Ok(named),
        Ok(None) => Err("no host named"),
        Err(NotText) => Err("the host's name is not UTF-8"),
    }
}

/// One of the configuration's strings, or the words that say it is not text.
///
/// # Errors
///
/// Those words, naming the field, when its bytes are not UTF-8.
///
/// # Safety
///
/// As [`text`].
unsafe fn setting<'held>(pointer: *const c_char, what: &str) -> Result<Option<&'held str>, String> {
    // SAFETY: the caller's obligation, above, which is `text`'s.
    unsafe { text(pointer) }.map_err(|NotText| format!("the configuration's {what} is not UTF-8"))
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
    let held = match configured(said) {
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

/// The options a configuration says, every string of it read as text.
///
/// # Errors
///
/// The words for a person when a string is not UTF-8 or the runtime paths
/// cannot be made.
fn configured(said: Option<&Configuration>) -> Result<ManagerOptions, String> {
    let Some(held) = said else {
        return options(None, None, None, None);
    };
    // SAFETY: the caller's obligation, stated on `iznik_client_new`: every
    // string of the configuration is null or null-terminated and alive for
    // the call, which is `setting`'s obligation.
    let runtime = unsafe { setting(held.runtime_directory, "runtime_directory") }?;
    // SAFETY: the same obligation, for the next of them.
    let artifacts = unsafe { setting(held.artifacts_directory, "artifacts_directory") }?;
    // SAFETY: the same again.
    let askpass = unsafe { setting(held.askpass_program, "askpass_program") }?;
    // SAFETY: and the last of them.
    let log = unsafe { setting(held.log_path, "log_path") }?;
    options(runtime, artifacts, askpass, log)
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
    let deliveries = Arc::new(Deliveries::default());
    let counting = Arc::clone(&deliveries);
    let pump = std::thread::spawn(move || deliver(&telling, &watching, &counting, &events));
    let delivers = pump.thread().id();
    Client {
        manager: Mutex::new(Some(Arc::new(manager))),
        listening,
        attached,
        deliveries,
        delivers,
        pump: Some(pump),
        calls: Calls::default(),
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
        | ManagerError::Alias { .. }
        | ManagerError::Log { .. }
        | ManagerError::NotCarrying { .. }
        | ManagerError::UnknownHost { .. }
        | ManagerError::Gone { .. }
        | ManagerError::Unsupported { .. }
        | ManagerError::Oversize { .. }
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
/// ended.
///
/// No callback begins once this has, and a call that arrives while it runs is
/// refused. Calls already under way — on any thread, a handler's among them —
/// are waited for, and so is a handler that is running, unless this is called
/// from inside a handler: then it returns without waiting for the handler it
/// is inside, no other callback is made, and the callback thread ends by
/// itself once that handler returns.
///
/// **Obligation:** the pointer came from [`iznik_client_new`], has not been
/// freed, and is not used afterwards — by the handler that called this, when a
/// handler did, as much as by anything else. A null pointer is nothing to free
/// and is ignored. Not called while holding a lock that a handler takes: it
/// waits for the handler that is running.
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
/// Replacing it, or setting it to null, returns at once: no callback begins
/// with the old one afterwards, and one already running may still be
/// finishing — `iznik_wait_for_callbacks` waits for it.
///
/// **Obligation:** whatever `context` points at outlives every callback made
/// with it: until the client is freed, or until the callback has been
/// replaced or set to null and `iznik_wait_for_callbacks` has returned. And it
/// may be used from the thread the callbacks arrive on, which is iznik's own
/// and not the one that called this.
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
    let Some(_call) = held.calls.enter() else {
        return;
    };
    let Ok(mut listening) = held.listening.lock() else {
        return;
    };
    listening.callback = callback;
    listening.context = Carried(context);
}

/// The version of the boundary the library was built for, which an
/// application compares with the `IZNIK_ABI_VERSION` of the header it was
/// built against.
#[unsafe(no_mangle)]
pub extern "C" fn iznik_abi_version() -> u32 {
    ABI_VERSION
}

/// This library's own version, for a log or an about box.
///
/// The string is iznik's, null-terminated, and valid for as long as the
/// library is loaded; it is never freed.
#[unsafe(no_mangle)]
pub extern "C" fn iznik_version() -> *const c_char {
    VERSION.as_ptr().cast::<c_char>()
}

/// Waits until every callback that had begun when this was called has
/// returned.
///
/// Letting a pane go, attaching over it and replacing the event callback all
/// return at once: none of them waits for a handler, because a handler may be
/// waiting for the very thread that called them. What each promises is that
/// no callback *begins* with what was taken away once it has returned. This is
/// how an application learns that the ones already running have finished, and
/// so that what it gave them may be freed. Called from the callback thread, it
/// returns at once: the handler running there is the caller.
///
/// **Obligation:** not called while holding a lock that a handler takes: it
/// waits for the handler that is running, which would be waiting for that
/// lock.
///
/// # Safety
///
/// `client` is a live client from [`iznik_client_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_wait_for_callbacks(client: *mut Client) {
    // SAFETY: the caller's obligation, above.
    let Some(held) = (unsafe { borrowed(client) }) else {
        return;
    };
    let Some(_call) = held.calls.enter() else {
        return;
    };
    held.quiesce();
}

/// Begins holding a host, and connecting to it.
///
/// Returns at once; what happens next arrives on the callback. An empty
/// alias, or one beginning with `-`, is refused with
/// `IZNIK_INVALID_ARGUMENT`: `ssh` would read it as an option.
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
    unsafe { with_host(client, alias, error, HostManager::add_host) }
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
            manager.upgrade(named, force, false)
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
/// The answer arrives as a `CommandResult` event carrying that number, and
/// what the command did arrives as a `Delta`. The events carry what the host
/// has said and only that — never a change this client is showing ahead of
/// it — so an application that wants a rename on the screen before the host
/// has agreed to it makes that change itself, and puts it back when the
/// `CommandResult` for its number says the host refused.
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
    let named = match unsafe { host_named(host) } {
        Ok(named) => named,
        Err(why) => {
            // SAFETY: the caller's obligation, above.
            unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, why) };
            return INVALID_ARGUMENT;
        }
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
    let Some((_call, manager)) = held.engaged() else {
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

/// Runs one operation that names a host, with the error filled in.
///
/// The operation runs on a share of the engine and under no lock of this
/// crate's, so an uninstall that takes minutes on one host leaves every other
/// call — on this host or another — free to run beside it.
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
    let named = match unsafe { host_named(alias) } {
        Ok(named) => named,
        Err(why) => {
            // SAFETY: the caller's obligation, above.
            unsafe { error::fill(error, INVALID_ARGUMENT, Layer::Client, why) };
            return INVALID_ARGUMENT;
        }
    };
    let Some((_call, manager)) = held.engaged() else {
        // SAFETY: the caller's obligation, above.
        unsafe { error::fill(error, REFUSED, Layer::Client, "the client is ending") };
        return REFUSED;
    };
    match doing(&manager, named) {
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
        // A name no host may have is a mistake in the argument, not a host
        // that said no.
        ManagerError::Alias { .. } | ManagerError::Oversize { .. } => INVALID_ARGUMENT,
        _otherwise => REFUSED,
    }
}
