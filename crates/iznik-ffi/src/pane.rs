//! A pane's bytes, straight into a surface.
//!
//! Everywhere else at this boundary iznik copies: a buffer it hands over is
//! valid for the callback and no longer, and a buffer handed in is read before
//! the call returns. Here is the one exception, and it is deliberate. A pane's
//! output is the hot path of the whole system — a program printing at the
//! speed of a link — and copying it once for the boundary and once for the
//! emulator underneath would be two copies to no purpose. The output callback
//! is handed the bytes where they are, and the application feeds them to its
//! surface before it returns.
//!
//! What pays for that is flow control, and it is not optional. An application
//! returns credit as its surface consumes; the host sends no more than it has
//! been given. One that returns none stalls its own pane and nothing else —
//! not the host, not another pane, not another host.

use core::ffi::{c_char, c_int, c_void};

use iznik_protocol::identity::PaneId;

use crate::error::{Error, INVALID_ARGUMENT, Layer};
use crate::{Client, DONE, borrowed, code_of, layer_of, text};

/// What iznik calls a pane's own handler with.
///
/// Every one of them arrives on the same thread every other event does, and
/// none is called while iznik holds a lock of its own.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PaneCallbacks {
    /// A pane's bytes, where they are.
    ///
    /// **Obligation:** the bytes are valid for this call only. Feed them to a
    /// surface before returning; do not keep the pointer.
    pub output: Option<extern "C" fn(context: *mut c_void, bytes: *const u8, length: usize)>,
    /// The pane's screen, as the bytes that reproduce it at `sequence`.
    ///
    /// **Obligation:** reset the surface to `columns` by `rows` and feed it
    /// these bytes before any further output. What came before them is gone.
    pub screen: Option<
        extern "C" fn(
            context: *mut c_void,
            sequence: u64,
            columns: u16,
            rows: u16,
            bytes: *const u8,
            length: usize,
        ),
    >,
    /// A shell-integration event, as an encoded `ToClient` at `sequence`.
    pub mark:
        Option<extern "C" fn(context: *mut c_void, sequence: u64, bytes: *const u8, length: usize)>,
    /// The host has stopped sending this pane's output.
    pub detached: Option<extern "C" fn(context: *mut c_void)>,
}

/// Begins delivery of a pane's output to these handlers.
///
/// **Obligation:** whatever `context` points at outlives the attachment — it
/// is detached, or the client is freed, before it goes away. Either is enough
/// on its own: both wait for a handler that is running before they return, so
/// the moment one of them answers, nothing is reading it any more.
///
/// # Safety
///
/// `client` is a live client, `host` a null-terminated string, and `error`
/// null or an [`Error`] the caller owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_pane_attach(
    client: *mut Client,
    host: *const c_char,
    pane: u64,
    callbacks: PaneCallbacks,
    context: *mut c_void,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, above.
    unsafe {
        with_pane(client, host, error, |held, named| {
            held.attach(named, PaneId(pane), callbacks, context);
            held.manager()
                .map_or(Ok(()), |manager| manager.subscribe(named, PaneId(pane)))
        })
    }
}

/// Ends it.
///
/// # Safety
///
/// As [`iznik_pane_attach`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_pane_detach(
    client: *mut Client,
    host: *const c_char,
    pane: u64,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_pane_attach`'s.
    let outcome = unsafe {
        with_pane(client, host, error, |held, named| {
            held.forget(named, PaneId(pane));
            held.manager()
                .map_or(Ok(()), |manager| manager.unsubscribe(named, PaneId(pane)))
        })
    };
    // The pane is out of reach now, so no call will begin for it; one that had
    // already begun is waited for here, with everything this took let go —
    // which is what makes the obligation above keepable: when this returns,
    // nothing is reading the context any more.
    // SAFETY: the caller's obligation: a live client, as above.
    if let Some(held) = unsafe { borrowed(client) } {
        held.quiesce();
    }
    outcome
}

/// Returns credit for what a surface has consumed.
///
/// The host sends no more than it has been given, so an application that never
/// calls this stalls its own pane and nothing else.
///
/// # Safety
///
/// As [`iznik_pane_attach`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_pane_credit(
    client: *mut Client,
    host: *const c_char,
    pane: u64,
    bytes: u32,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_pane_attach`'s.
    unsafe {
        with_pane(client, host, error, |held, named| {
            held.manager()
                .map_or(Ok(()), |manager| manager.credit(named, PaneId(pane), bytes))
        })
    }
}

/// Sends keystrokes, a paste, or an emulator's own answer to a program's
/// query.
///
/// One call is one message on the wire, so a bracketed paste arrives as a
/// paste rather than as the keys it happens to contain.
///
/// **Obligation:** the bytes are read before this returns and may be freed as
/// soon as it does.
///
/// # Safety
///
/// As [`iznik_pane_attach`], and `bytes` points at `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_pane_input(
    client: *mut Client,
    host: *const c_char,
    pane: u64,
    bytes: *const u8,
    length: usize,
    error: *mut Error,
) -> c_int {
    if bytes.is_null() {
        // SAFETY: the caller's obligation, above.
        unsafe { crate::error::fill(error, INVALID_ARGUMENT, Layer::Client, "no bytes given") };
        return INVALID_ARGUMENT;
    }
    // SAFETY: the caller's obligation: `length` readable bytes for this call,
    // which is where they are copied.
    let typed = unsafe { core::slice::from_raw_parts(bytes, length) }.to_vec();
    // SAFETY: the caller's obligations, as `iznik_pane_attach`'s.
    unsafe {
        with_pane(client, host, error, move |held, named| {
            held.manager()
                .map_or(Ok(()), |manager| manager.input(named, PaneId(pane), typed))
        })
    }
}

/// Tells a pane it is another size.
///
/// The application decides the size; every client sees the result as a
/// `PaneResized` change.
///
/// # Safety
///
/// As [`iznik_pane_attach`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_pane_resize(
    client: *mut Client,
    host: *const c_char,
    pane: u64,
    columns: u16,
    rows: u16,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_pane_attach`'s.
    unsafe {
        with_pane(client, host, error, |held, named| {
            held.manager().map_or(Ok(()), |manager| {
                manager.resize(named, PaneId(pane), columns, rows)
            })
        })
    }
}

/// Names the pane the person is looking at.
///
/// # Safety
///
/// As [`iznik_pane_attach`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iznik_pane_focus(
    client: *mut Client,
    host: *const c_char,
    pane: u64,
    error: *mut Error,
) -> c_int {
    // SAFETY: the caller's obligations, as `iznik_pane_attach`'s.
    unsafe {
        with_pane(client, host, error, |held, named| {
            held.manager()
                .map_or(Ok(()), |manager| manager.focus(named, Some(PaneId(pane))))
        })
    }
}

/// Runs one operation that names a host, with the calls serialized and the
/// error filled in.
///
/// # Safety
///
/// As [`iznik_pane_attach`].
unsafe fn with_pane(
    client: *mut Client,
    host: *const c_char,
    error: *mut Error,
    doing: impl FnOnce(&Client, &str) -> Result<(), iznik_client::host::manager::ManagerError>,
) -> c_int {
    // SAFETY: the caller's obligation, above.
    unsafe { crate::error::clear(error) };
    // SAFETY: the caller's obligation, above.
    let Some(held) = (unsafe { borrowed(client) }) else {
        // SAFETY: the caller's obligation, above.
        unsafe { crate::error::fill(error, INVALID_ARGUMENT, Layer::Client, "no client") };
        return INVALID_ARGUMENT;
    };
    // SAFETY: the caller's obligation, above.
    let Some(named) = (unsafe { text(host) }) else {
        // SAFETY: the caller's obligation, above.
        unsafe { crate::error::fill(error, INVALID_ARGUMENT, Layer::Client, "no host named") };
        return INVALID_ARGUMENT;
    };
    let Some(_serialized) = held.serialize() else {
        // SAFETY: the caller's obligation, above.
        unsafe {
            crate::error::fill(
                error,
                crate::error::REFUSED,
                Layer::Client,
                "the client is broken",
            );
        };
        return crate::error::REFUSED;
    };
    match doing(held, named) {
        Ok(()) => DONE,
        Err(refusal) => {
            // SAFETY: the caller's obligation, above.
            unsafe {
                crate::error::fill(
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
