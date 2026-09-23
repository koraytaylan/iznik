//! How what the manager says reaches the application: the one thread every
//! callback arrives on, what it looks up before each call, and the count that
//! lets a caller wait for the calls already made without anything being held
//! while they run.

use core::ffi::c_void;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::{ToClient, encode_to_client};

use crate::model::{Event, EventCallback};
use crate::pane::PaneCallbacks;
use crate::shape;

/// A pointer the application gave and iznik carries back to it untouched.
///
/// The application decides what it means; this only has to hold it and hand
/// it to the one thread that calls the callback.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Carried(pub(crate) *mut c_void);

// SAFETY: what is carried is opaque here — nothing in this library reads or
// writes through it; it is only handed back. The obligation the header states
// on both `iznik_set_event_callback` and `iznik_pane_attach` — whatever
// `context` points at "may be used from the thread the callbacks arrive on,
// which is iznik's own and not the one that called this" — is what makes
// moving the pointer to that thread sound, and that move is all this allows.
unsafe impl Send for Carried {}

/// What the application asked to be told, and what to tell it with.
#[derive(Debug)]
pub(crate) struct Listener {
    /// What to call.
    pub(crate) callback: EventCallback,
    /// What to pass it.
    pub(crate) context: Carried,
}

/// One pane an application is watching.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attached {
    /// What to call.
    pub(crate) callbacks: PaneCallbacks,
    /// What to pass it.
    pub(crate) context: Carried,
}

/// How many callbacks have begun, and how many of those have returned.
///
/// Counted rather than locked: a lock held while a callback runs is a lock an
/// application's own thread waits on while the handler waits on the
/// application's lock, and neither ever moves again. A callback is counted
/// as begun in the same breath as what it will be given is looked up, under
/// the lock that guards what it looks up — so anything taken away after that
/// lock is let go is never handed to a callback that begins afterwards, and
/// one that had already begun is counted in what a wait waits for.
#[derive(Debug, Default)]
pub(crate) struct Deliveries {
    /// The two counts.
    counts: Mutex<Counts>,
    /// Signalled whenever a callback returns.
    returned: Condvar,
}

/// The counts [`Deliveries`] keeps.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    /// Callbacks that have begun.
    begun: u64,
    /// Callbacks that have returned.
    ended: u64,
    /// Whether the client is being freed, which ends every wait: the
    /// callback a wait would be waiting for may be the one freeing it.
    ending: bool,
}

impl Deliveries {
    /// Says one callback is about to be made.
    pub(crate) fn begin(&self) {
        if let Ok(mut held) = self.counts.lock() {
            held.begun = held.begun.saturating_add(1);
        }
    }

    /// Says it has returned.
    pub(crate) fn end(&self) {
        if let Ok(mut held) = self.counts.lock() {
            held.ended = held.ended.saturating_add(1);
        }
        self.returned.notify_all();
    }

    /// Waits until every callback that had begun when this was called has
    /// returned.
    pub(crate) fn wait(&self) {
        let Ok(held) = self.counts.lock() else {
            return;
        };
        let target = held.begun;
        let _done = self
            .returned
            .wait_while(held, |counts| counts.ended < target && !counts.ending);
    }

    /// Ends every wait there is and every one to come, because the client is
    /// being freed.
    pub(crate) fn close(&self) {
        if let Ok(mut held) = self.counts.lock() {
            held.ending = true;
        }
        self.returned.notify_all();
    }
}

/// Reads every event and hands it to the application, on this one thread.
///
/// Nothing of this crate's is held while a callback runs: what to call is read
/// under its lock, the callback is counted as begun under that same lock, and
/// the lock is let go before the call. So a handler may call straight back in,
/// and an application thread that takes a context away never waits for a
/// handler — which may itself be waiting for that thread.
pub(crate) fn deliver(
    listening: &Arc<Mutex<Listener>>,
    attached: &Arc<Mutex<BTreeMap<(HostId, PaneId), Attached>>>,
    deliveries: &Deliveries,
    events: &Receiver<ManagerEvent>,
) {
    while let Ok(event) = events.recv() {
        // A pane somebody is watching is handed its own bytes, and not handed
        // them twice: an application that attached to a pane reads it there.
        if handed(attached, deliveries, &event) {
            continue;
        }
        let Some((callback, context)) = listened(listening, deliveries) else {
            continue;
        };
        carry(callback, context, &event);
        deliveries.end();
    }
}

/// The event callback and its context, counted as begun, when there is one.
pub(crate) fn listened(
    listening: &Arc<Mutex<Listener>>,
    deliveries: &Deliveries,
) -> Option<(extern "C" fn(*const Event, *mut c_void), Carried)> {
    let held = listening.lock().ok()?;
    let callback = held.callback?;
    deliveries.begin();
    Some((callback, held.context))
}

/// Hands one event to the pane it is about, when somebody is watching that
/// pane with a handler for it, and says whether it did.
///
/// Looked up, and counted as begun, under the lock a detachment takes: a pane
/// let go before this looked is never handed anything again, and one let go
/// after is waited for by whoever waits for callbacks.
pub(crate) fn handed(
    attached: &Arc<Mutex<BTreeMap<(HostId, PaneId), Attached>>>,
    deliveries: &Deliveries,
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
    if !handles(&watching, event) {
        return false;
    }
    deliveries.begin();
    drop(held);
    let _taken = to_the_pane(&watching, event);
    deliveries.end();
    true
}

/// Whether a pane's handlers include one for this event.
pub(crate) fn handles(watching: &Attached, event: &ManagerEvent) -> bool {
    let callbacks = &watching.callbacks;
    match event {
        ManagerEvent::Bytes { .. } => callbacks.output.is_some(),
        ManagerEvent::Screen { .. } => callbacks.screen.is_some(),
        ManagerEvent::Detached { .. } => callbacks.detached.is_some(),
        ManagerEvent::Notify(Notification::Mark { .. }) => callbacks.mark.is_some(),
        _elsewhere => false,
    }
}

/// Which host and pane an event is about, when it is about one.
pub(crate) fn about(event: &ManagerEvent) -> Option<(HostId, PaneId)> {
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
pub(crate) fn to_the_pane(watching: &Attached, event: &ManagerEvent) -> bool {
    let context = watching.context.0;
    match event {
        ManagerEvent::Bytes { bytes, .. } => match watching.callbacks.output {
            Some(output) => {
                output(context, held_or_null(bytes), bytes.len());
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
                    held_or_null(bytes),
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
pub(crate) fn marked(
    watching: &Attached,
    notification: &Notification,
    context: *mut c_void,
) -> bool {
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
    mark(context, sequence.0, held_or_null(&payload), payload.len());
    true
}

/// Where some bytes begin, or null when there are none.
///
/// An empty `Vec` answers a pointer that is aligned and not null and not
/// anything either — which a C caller testing `if (event->payload)` would take
/// for bytes and read. No bytes is null, which is what that test is asking.
pub(crate) fn held_or_null(bytes: &[u8]) -> *const u8 {
    if bytes.is_empty() {
        return core::ptr::null();
    }
    bytes.as_ptr()
}

/// Hands one event over, with every pointer in it alive for exactly the call.
pub(crate) fn carry(
    callback: extern "C" fn(*const Event, *mut c_void),
    context: Carried,
    event: &ManagerEvent,
) {
    let Some(shaped) = shape::shaped(event) else {
        return;
    };
    let Ok(named) = CString::new(shaped.host) else {
        return;
    };
    let held = Event {
        kind: shaped.kind,
        host: named.as_ptr(),
        pane: shaped.pane,
        sequence: shaped.sequence,
        columns: shaped.columns,
        rows: shaped.rows,
        generation: shaped.generation,
        command_id: shaped.command,
        payload: held_or_null(&shaped.payload),
        payload_length: shaped.payload.len(),
    };
    callback(&raw const held, context.0);
}
