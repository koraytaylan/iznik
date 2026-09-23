//! What the boundary does when calls overlap: one slow call beside quick
//! ones, a callback beside the calls that take its context away, and the
//! client ending while calls are still under way.
//!
//! Every case goes through the `extern "C"` functions, from as many threads
//! as the case needs, and what is asserted is what an application would see.

use core::time::Duration;
use std::ffi::CString;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use iznik::error::{Error, Layer, OK};
use iznik::{
    Client, Configuration, iznik_client_free, iznik_client_new, iznik_host_add, iznik_host_remove,
};

/// How long a quick call may take while a slow one is running beside it.
const QUICK: Duration = Duration::from_secs(1);

/// How long a case lets a slow call get under way before calling beside it.
const UNDER_WAY: Duration = Duration::from_millis(200);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

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
    let path = base.join(format!("iznik-calls-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// An error nothing has been written to yet.
fn blank() -> Error {
    Error {
        code: OK,
        layer: Layer::Client,
        message: core::ptr::null(),
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
