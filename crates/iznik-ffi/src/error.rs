//! What a call says when it will not do what it was asked.
//!
//! Two things matter here and nothing else does. The layer, because a person
//! debugging a system that spans an application, an engine, an SSH link, a
//! daemon and a shell needs to know which of them is broken before anything
//! else. And the message, because it is written for a person to read: the
//! application may put it on the screen without rewording it.

use core::ffi::{c_char, c_int};
use std::cell::RefCell;
use std::ffi::CString;

/// The code a call gives when nothing went wrong.
pub const OK: c_int = 0;

/// Something the application passed in was not what it said it was: a null
/// where one is not allowed, or bytes that are not UTF-8.
pub const INVALID_ARGUMENT: c_int = 1;

/// No host of that name is held.
pub const UNKNOWN_HOST: c_int = 2;

/// The host, or the machine this runs on, refused what was asked.
pub const REFUSED: c_int = 3;

/// Which layer of the stack a failure came from.
///
/// The first question anybody asks about a system this tall.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// The link to the host: `ssh`, a socket, a network.
    Transport = 0,
    /// Getting a server onto the host, or starting it.
    Bootstrap = 1,
    /// What the host's own daemon said.
    Server = 2,
    /// The wire between them.
    Protocol = 3,
    /// This engine, on this machine.
    Client = 4,
}

/// Why a call did not do what it was asked.
///
/// The application allocates it and passes a pointer; iznik fills it in. The
/// message points at storage this library owns and keeps until the next call
/// on the same thread — an application that needs it longer copies it, which
/// is the same rule every other buffer here follows.
#[repr(C)]
#[derive(Debug)]
pub struct Error {
    /// Zero when nothing went wrong, and one of the codes above otherwise.
    pub code: c_int,
    /// Which layer it came from.
    pub layer: Layer,
    /// What went wrong, as UTF-8 the application may display verbatim.
    pub message: *const c_char,
}

thread_local! {
    /// The last message this thread was given, kept alive for exactly as long
    /// as the promise above says.
    static SAID: RefCell<CString> = RefCell::new(CString::default());
}

/// Puts a message where the pointer handed back will stay valid, and gives
/// back that pointer.
///
/// Bytes that cannot be a C string — a message with a null in it, which no
/// error of this program's makes — become an empty one rather than a failure
/// about a failure.
fn remembered(message: &str) -> *const c_char {
    SAID.with(|held| {
        let said = CString::new(message).unwrap_or_default();
        let pointer = said.as_ptr();
        *held.borrow_mut() = said;
        pointer
    })
}

/// Fills in an error, when the application asked for one.
///
/// # Safety
///
/// `error` is null or points at an [`Error`] the caller owns and may write.
pub unsafe fn fill(error: *mut Error, code: c_int, layer: Layer, message: &str) {
    if error.is_null() {
        return;
    }
    let held = Error {
        code,
        layer,
        message: remembered(message),
    };
    // SAFETY: the caller's obligation, above: `error` is not null here and
    // points at an `Error` it owns, so writing one is writing to its own
    // storage.
    unsafe { error.write(held) };
}

/// Fills in an error that says nothing went wrong.
///
/// # Safety
///
/// As [`fill`].
pub unsafe fn clear(error: *mut Error) {
    // SAFETY: the caller's obligation is `fill`'s, and this is a call to it.
    unsafe { fill(error, OK, Layer::Client, "") };
}
