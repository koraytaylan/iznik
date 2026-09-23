//! What the boundary does when calls overlap: one slow call beside quick
//! ones, a callback beside the calls that take its context away, and the
//! client ending while calls are still under way.
//!
//! Every case goes through the `extern "C"` functions, from as many threads
//! as the case needs, and what is asserted is what an application would see.

#[path = "fixtures/pane_daemon.rs"]
mod daemon;

use core::ffi::{c_int, c_void};
use core::time::Duration;
use std::ffi::CString;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iznik::error::OK;
use iznik::model::Event;
use iznik::pane::{PaneCallbacks, iznik_pane_attach, iznik_pane_detach};
use iznik::{
    Client, Configuration, iznik_client_free, iznik_client_new, iznik_host_add, iznik_host_remove,
    iznik_set_event_callback, iznik_wait_for_callbacks,
};

use daemon::{PANE, PROMPT, Scratch, blank, connected, runtime, said, scratch, typed};

/// How long a quick call may take while a slow one is running beside it.
const QUICK: Duration = Duration::from_secs(1);

/// How long a case lets a slow call get under way before calling beside it.
const UNDER_WAY: Duration = Duration::from_millis(200);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// How long a case waits to be sure something is *not* going to happen.
const BRIEF: Duration = Duration::from_millis(400);

/// How long the handler in the case about waiting holds on for.
const DAWDLE: Duration = Duration::from_millis(400);

/// How long a wait for that handler must have taken to have waited at all, and
/// how long letting the pane go may take and still have waited for nothing.
///
/// Well under the whole, since the case notices the handler some way into it;
/// far above nothing, which is what a call that did not wait would take.
const WAITED: Duration = Duration::from_millis(100);

/// What the case about waiting passes to its handler: how many are inside it.
///
/// A count rather than a flag that one has begun: what has to be caught is a
/// handler running *now*, and one that has been and gone would look the same.
struct Dawdling(Mutex<usize>);

/// A handler that takes its time.
extern "C" fn dawdles(context: *mut c_void, _bytes: *const u8, _length: usize) {
    if context.is_null() {
        return;
    }
    // SAFETY: this case's own box, alive until the case ends.
    let dawdling = unsafe { &*context.cast::<Dawdling>() };
    if let Ok(mut inside) = dawdling.0.lock() {
        *inside = inside.saturating_add(1);
    }
    std::thread::sleep(DAWDLE);
    if let Ok(mut inside) = dawdling.0.lock() {
        *inside = inside.saturating_sub(1);
    }
}

/// What the case that lets a pane go from inside a handler passes to it.
struct Leaving {
    /// The client to call back into.
    client: Shared,
    /// The host, kept null-terminated for that call.
    alias: CString,
    /// What the handler has been told, and what came of its own call.
    told: Mutex<Left>,
}

/// What that handler saw.
#[derive(Debug, Default)]
struct Left {
    /// How many times output arrived.
    calls: usize,
    /// What letting the pane go answered, once it has.
    answered: Option<c_int>,
}

/// A handler that lets its own pane go, from inside the call.
///
/// The one call that must not wait for the call it is inside: waiting for a
/// handler is what lets an application free what it gave, and a handler doing
/// it to itself would wait for ever.
extern "C" fn leaves(context: *mut c_void, _bytes: *const u8, _length: usize) {
    if context.is_null() {
        return;
    }
    // SAFETY: the context is this case's own box, alive until the case ends.
    let leaving = unsafe { &*context.cast::<Leaving>() };
    let first = {
        let Ok(mut told) = leaving.told.lock() else {
            return;
        };
        told.calls = told.calls.saturating_add(1);
        told.calls == 1
    };
    if !first {
        return;
    }
    // SAFETY: the client is live for the whole of the case and the alias is
    // null-terminated; no error is asked for.
    let answered = unsafe {
        iznik_pane_detach(
            leaving.client.0,
            leaving.alias.as_ptr(),
            PANE,
            core::ptr::null_mut(),
        )
    };
    if let Ok(mut told) = leaving.told.lock() {
        told.answered = Some(answered);
    }
}

/// A client whose runtime files go under `held`.
///
/// # Errors
///
/// When it cannot be made.
fn client(held: &Scratch) -> Result<Shared, Failed> {
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
        return Err("no client".into());
    }
    Ok(Shared(made))
}

/// A client pointer a case hands to another of its threads, which the header
/// allows: every function may be called from any thread.
#[derive(Clone, Copy, Debug)]
struct Shared(*mut Client);

// SAFETY: the header's own promise, that every function is safe to call from
// any thread; the case frees the client only once every thread it gave the
// pointer to has been joined.
unsafe impl Send for Shared {}

impl Shared {
    /// The pointer, taken by value so a closure captures the whole wrapper.
    fn pointer(self) -> *mut Client {
        self.0
    }
}

/// The alias of a socket.
///
/// # Errors
///
/// When it holds a nul.
fn alias(socket: &std::path::Path) -> Result<CString, Failed> {
    Ok(CString::new(format!("unix:{}", socket.display()))?)
}

/// # Panics
///
/// When a call that takes seconds holds up a quick call beside it.
#[test]
fn concurrent_calls_do_not_wait_behind_a_slow_one() {
    let case = || -> Result<(), Failed> {
        let held = scratch("slow")?;
        let made = client(&held)?;
        // A socket that answers the connection and then never says hello:
        // the host's task sits inside its bootstrap, and letting it go waits
        // for that.
        let socket = held.path.join("silent.sock");
        let listener = UnixListener::bind(&socket)?;
        let stuck = alias(&socket)?;
        let mut error = blank();
        // SAFETY: the client is live and the alias null-terminated.
        let added = unsafe { iznik_host_add(made.pointer(), stuck.as_ptr(), &raw mut error) };
        assert_eq!(added, OK);
        let (answered, _from) = listener.accept()?;
        let finished = Arc::new(AtomicBool::new(false));
        let noticing = Arc::clone(&finished);
        let removing = stuck.clone();
        let slow = std::thread::spawn(move || {
            let mut refusal = blank();
            // SAFETY: the client is live until the case joins this thread,
            // and the alias is null-terminated.
            let _gone =
                unsafe { iznik_host_remove(made.pointer(), removing.as_ptr(), &raw mut refusal) };
            noticing.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(UNDER_WAY);
        let elsewhere = alias(&held.path.join("elsewhere.sock"))?;
        let started = Instant::now();
        // SAFETY: the client is live and the alias null-terminated.
        let quick = unsafe { iznik_host_add(made.pointer(), elsewhere.as_ptr(), &raw mut error) };
        let took = started.elapsed();
        assert_eq!(quick, OK);
        assert!(
            !finished.load(Ordering::SeqCst),
            "the slow call was still running beside the quick one"
        );
        assert!(
            took < QUICK,
            "and the quick one did not wait for it: {took:?}"
        );
        // What the stuck host was waiting for goes, and the slow call ends.
        drop(answered);
        slow.join().map_err(|_panicked| "the slow call panicked")?;
        // SAFETY: it came from `iznik_client_new`, every thread given it has
        // been joined, and it is freed once, here.
        unsafe { iznik_client_free(made.pointer()) };
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When letting a pane go from inside its own handler waits for that handler
/// to finish, or when anything arrives for it afterwards.
#[test]
fn concurrent_calls_let_a_pane_go_from_inside_its_own_handler() {
    let case = || -> Result<(), Failed> {
        let held = scratch("leaving")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let leaving: *mut Leaving = Box::into_raw(Box::new(Leaving {
            client: Shared(client),
            alias: alias.clone(),
            told: Mutex::new(Left::default()),
        }));
        let handlers = PaneCallbacks {
            output: Some(leaves),
            screen: None,
            mark: None,
            detached: None,
        };
        let mut error = blank();
        // SAFETY: the client is live, the alias null-terminated, and the
        // context outlives the attachment — this case frees it at the end.
        let taken = unsafe {
            iznik_pane_attach(
                client,
                alias.as_ptr(),
                PANE,
                handlers,
                leaving.cast::<c_void>(),
                &raw mut error,
            )
        };
        assert_eq!(taken, OK, "the pane was taken: {}", said(&error));
        typed(client, &alias, PANE, "#hello\n")?;
        // The handler lets the pane go while it is running. Without the rule
        // that a handler waits for nothing, this never answers.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        while Instant::now() < expires {
            // SAFETY: this case's own box, alive here.
            let answered = unsafe { &*leaving }
                .told
                .lock()
                .ok()
                .and_then(|told| told.answered);
            if answered.is_some() {
                assert_eq!(answered, Some(OK), "and it answers that it let go");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let calls = {
            // SAFETY: this case's own box, alive here.
            let told = unsafe { &*leaving }
                .told
                .lock()
                .map_err(|_broken| "the record")?;
            assert_eq!(told.answered, Some(OK), "letting go answered");
            told.calls
        };
        // And nothing more arrives for a pane that was let go, however much
        // the shell goes on saying.
        typed(client, &alias, PANE, "#more\n")?;
        std::thread::sleep(BRIEF);
        {
            // SAFETY: this case's own box, alive here.
            let told = unsafe { &*leaving }
                .told
                .lock()
                .map_err(|_broken| "the record")?;
            assert_eq!(told.calls, calls, "nothing arrived after it let go");
        }
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once, after the client
        // that could have called into it is gone.
        drop(unsafe { Box::from_raw(leaving) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When letting a pane go waits for a handler, or waiting for callbacks
/// answers while one is still reading what the application gave it.
#[test]
fn concurrent_calls_let_a_pane_go_at_once_and_wait_when_asked() {
    let case = || -> Result<(), Failed> {
        let held = scratch("waiting")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let dawdling: *mut Dawdling = Box::into_raw(Box::new(Dawdling(Mutex::new(0))));
        let handlers = PaneCallbacks {
            output: Some(dawdles),
            screen: None,
            mark: None,
            detached: None,
        };
        let mut error = blank();
        // SAFETY: the client is live, the alias null-terminated, and the
        // context outlives the attachment — this case frees it at the end.
        let taken = unsafe {
            iznik_pane_attach(
                client,
                alias.as_ptr(),
                PANE,
                handlers,
                dawdling.cast::<c_void>(),
                &raw mut error,
            )
        };
        assert_eq!(taken, OK, "the pane was taken: {}", said(&error));
        typed(client, &alias, PANE, "#hello\n")?;
        // Waited for closely, so that most of the handler's own wait is still
        // ahead of it when the letting go begins.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        let mut inside = false;
        while Instant::now() < expires && !inside {
            // SAFETY: this case's own box, alive here.
            inside = unsafe { &*dawdling }
                .0
                .lock()
                .is_ok_and(|running| *running > 0);
            if !inside {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert!(inside, "a handler is running, which is what is waited for");
        let started = Instant::now();
        // SAFETY: the client is live and the alias null-terminated.
        let gone = unsafe { iznik_pane_detach(client, alias.as_ptr(), PANE, &raw mut error) };
        let took = started.elapsed();
        assert_eq!(gone, OK, "the pane was let go: {}", said(&error));
        assert!(took < WAITED, "letting go waited for no handler: {took:?}");
        // SAFETY: the client is live.
        unsafe { iznik_wait_for_callbacks(client) };
        let waited = started.elapsed();
        assert!(waited >= WAITED, "waiting waited for it: {waited:?}");
        // SAFETY: this case's own box, alive here.
        let still = unsafe { &*dawdling }.0.lock().map(|running| *running);
        assert_eq!(still.ok(), Some(0), "and nothing is reading it now");
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once, after the client
        // that could have called into it is gone.
        drop(unsafe { Box::from_raw(dawdling) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// What the case about a handler that takes the application's lock shares
/// with that handler.
#[derive(Default)]
struct Contended {
    /// The application's own lock, which the handler takes.
    lock: Mutex<()>,
    /// Whether the handler has begun, and is about to wait for that lock.
    begun: AtomicBool,
}

/// An event handler that takes the application's own lock, as the contract
/// says a handler may.
extern "C" fn contends(_event: *const Event, context: *mut c_void) {
    if context.is_null() {
        return;
    }
    // SAFETY: this case's own box, alive until the case ends.
    let contended = unsafe { &*context.cast::<Contended>() };
    contended.begun.store(true, Ordering::SeqCst);
    drop(contended.lock.lock());
}

/// # Panics
///
/// When taking the event callback away waits for a handler that is waiting
/// for the lock the application holds while it takes it away — the deadlock
/// the boundary must not have — or waiting for callbacks, once that lock is
/// let go, does not wait for the handler to finish.
#[test]
fn concurrent_calls_take_a_callback_away_while_its_handler_waits() {
    let case = || -> Result<(), Failed> {
        let held = scratch("contended")?;
        let made = client(&held)?;
        let contended: *mut Contended = Box::into_raw(Box::new(Contended::default()));
        // SAFETY: the client is live and the context outlives every callback
        // made with it: the case waits for them before freeing it.
        unsafe {
            iznik_set_event_callback(made.pointer(), Some(contends), contended.cast::<c_void>());
        }
        // SAFETY: this case's own box, alive here.
        let shared = unsafe { &*contended };
        let holding = shared.lock.lock().map_err(|_broken| "a broken lock")?;
        let nowhere = CString::new(format!("unix:{}", held.path.join("nowhere.sock").display()))?;
        let mut error = blank();
        // SAFETY: the client is live and the alias null-terminated.
        let added = unsafe { iznik_host_add(made.pointer(), nowhere.as_ptr(), &raw mut error) };
        assert_eq!(added, OK, "{}", said(&error));
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        while !shared.begun.load(Ordering::SeqCst) && Instant::now() < expires {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            shared.begun.load(Ordering::SeqCst),
            "a handler is waiting for the lock"
        );
        // With the lock held, and the handler waiting for it.
        let started = Instant::now();
        // SAFETY: the client is live.
        unsafe { iznik_set_event_callback(made.pointer(), None, core::ptr::null_mut()) };
        assert!(
            started.elapsed() < BRIEF,
            "taking it away waited for nothing"
        );
        drop(holding);
        // SAFETY: the client is live, and this thread holds no lock a handler
        // takes.
        unsafe { iznik_wait_for_callbacks(made.pointer()) };
        assert!(
            shared.lock.try_lock().is_ok(),
            "and once waited for, the handler has let the lock go"
        );
        // SAFETY: it came from `iznik_client_new` and is freed once, here.
        unsafe { iznik_client_free(made.pointer()) };
        // SAFETY: the box this case made, taken back once nothing can call
        // into it.
        drop(unsafe { Box::from_raw(contended) });
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
