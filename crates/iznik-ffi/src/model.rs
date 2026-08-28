//! What the application is told, in the protocol's own encoding.
//!
//! There is one schema in this system, not two. An event carries the same
//! bytes the wire carried, so an application decodes a snapshot or a delta
//! with `iznik-protocol` — the schema the server generated it against — and
//! nothing here can drift from it, because there is nothing here to drift.
//!
//! Every buffer an event carries is valid for exactly as long as the callback
//! runs. An application that needs it afterwards copies it while it has it.

use core::ffi::{c_char, c_void};

/// What an event is about, and how to read its payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A host moved from one state to another. The payload is the state as
    /// UTF-8, for a person to read.
    HostState = 0,
    /// The host's whole model. The payload is a `HostModel` as
    /// `iznik_protocol::model::decode_host_model` reads it.
    Snapshot = 1,
    /// One numbered change. The payload is a `Delta` as
    /// `iznik_protocol::delta::decode_delta` reads it.
    Delta = 2,
    /// The answer to a command. The payload is a `CommandOutcome` as
    /// `iznik_protocol::command::decode_command_outcome` reads it, and the
    /// command's own number is in `command_id`.
    CommandResult = 3,
    /// A shell-integration event. The payload is a `ToClient` as
    /// `iznik_protocol::message::decode_to_client` reads it, which is a
    /// `Mark` carrying the pane, the sequence and what happened.
    Mark = 4,
    /// Something worth telling a person, as UTF-8.
    Notification = 5,
    /// A pane's own bytes, straight from the host. The pane is in `pane` and
    /// the byte the first of them is, is in `sequence`.
    PaneBytes = 6,
    /// A pane's screen, as the bytes that reproduce it. The pane is in `pane`
    /// and the byte it is exact at, is in `sequence`.
    Screen = 7,
}

/// One thing that happened.
///
/// Every pointer in it is valid for the duration of the callback and no
/// longer.
#[repr(C)]
#[derive(Debug)]
pub struct Event {
    /// What it is about.
    pub kind: EventKind,
    /// The host it is about, as the user's own alias, UTF-8.
    pub host: *const c_char,
    /// The pane it is about, for the kinds that name one; zero otherwise.
    pub pane: u64,
    /// Where in a pane's stream it sits, for the kinds that say; zero
    /// otherwise.
    pub sequence: u64,
    /// This client's number for a command, for `CommandResult`; zero
    /// otherwise.
    pub command_id: u64,
    /// The bytes, whose meaning `kind` decides.
    pub payload: *const u8,
    /// How many of them there are.
    pub payload_length: usize,
}

/// What iznik calls when something happens.
///
/// It is called on one thread and only that thread, so an application that
/// keeps state in its handler needs no lock of its own for it. It may call
/// back into iznik: the calls the application makes are serialized among
/// themselves, and nothing holds that lock while a callback runs.
pub type EventCallback = Option<extern "C" fn(event: *const Event, context: *mut c_void)>;
