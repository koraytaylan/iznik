//! Engine notifications retained by the concurrent pane-input fixture.

use super::watching;
use core::ffi::c_void;
use iznik::model::{Event, EventKind};

/// Record engine failures without tracing each byte or changing the input path.
pub(super) extern "C" fn record(event: *const Event, context: *mut c_void) {
    if event.is_null() {
        return;
    }
    // SAFETY: the event callback contract keeps this event live for this call.
    let event = unsafe { &*event };
    if !matches!(event.kind, EventKind::Notification | EventKind::HostState) {
        return;
    }
    let Some(watched) = watching(context) else {
        return;
    };
    let payload = if event.payload.is_null() {
        &[]
    } else {
        // SAFETY: the event callback contract guarantees this readable payload for this call.
        unsafe { core::slice::from_raw_parts(event.payload, event.payload_length) }
    };
    if let Ok(mut held) = watched.lock() {
        held.notices.push(format!(
            "{:?}: {}",
            event.kind,
            String::from_utf8_lossy(payload)
        ));
    }
}
