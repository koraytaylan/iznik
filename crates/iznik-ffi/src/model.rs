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

use crate::error::Layer;

/// What an event is about, and how to read its payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A host moved from one state to another. The payload is the state as
    /// UTF-8, for a person to read; a `HostStatus` event follows with the same
    /// move for a program.
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
    ///
    /// When it is about a command — one the host never answered, or one whose
    /// answer could not be read — that command's own number is in
    /// `command_id`, so whoever is waiting on it is released rather than left
    /// waiting on an answer that will not come in the shape they expected.
    Notification = 5,
    /// A pane's own bytes, straight from the host. The pane is in `pane` and
    /// the byte the first of them is, is in `sequence`.
    PaneBytes = 6,
    /// A pane's screen, as the bytes that reproduce it. The pane is in `pane`,
    /// the byte it is exact at is in `sequence`, and the size to reset a
    /// surface to before feeding them is in `columns` and `rows`.
    Screen = 7,
    /// The host has stopped sending a pane's output. The pane is in `pane`,
    /// and there is no payload: a pane that has gone is not a state anybody
    /// reads, it is a pane nothing more will arrive for.
    PaneDetached = 8,
    /// The same move as the `HostState` just before it, for a program to act
    /// on rather than a person to read. The payload points at one
    /// `iznik_host_status`, and `payload_length` is its size.
    HostStatus = 9,
}

/// Where a host is, as a program reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostStateKind {
    /// Nothing is being done about it.
    Disconnected = 0,
    /// It is being asked what it is.
    Probing = 1,
    /// A server is being put on it.
    Bootstrapping = 2,
    /// Its server is being started and greeted.
    Connecting = 3,
    /// Its server is being replaced.
    Upgrading = 4,
    /// It is connected.
    Connected = 5,
    /// Its link went, and it will be tried again.
    Reconnecting = 6,
    /// It could not be reached.
    Failed = 7,
    /// It is no longer held at all.
    Removed = 8,
}

/// What kind of failure a host is in, which says whether waiting helps.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// It is not failing.
    None = 0,
    /// Something that may pass by itself; it is tried again.
    Transient = 1,
    /// The host refused the credentials offered. Not tried again until
    /// somebody asks.
    Credentials = 2,
    /// The host's key is not one this machine accepts — unknown, or changed.
    /// Not tried again until somebody asks.
    HostKey = 3,
    /// The host is a machine this build carries no server for. Not tried
    /// again until somebody asks.
    Unsupported = 4,
}

/// Why an upgrade is on offer.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeKind {
    /// None is.
    None = 0,
    /// The host runs another version than this build carries.
    Version = 1,
    /// The host runs this version, missing features this build has.
    Capabilities = 2,
}

/// A host's state, as a `HostStatus` event carries it.
///
/// Every pointer in it is valid for the duration of the callback and no
/// longer.
#[repr(C)]
#[derive(Debug)]
pub struct HostStatus {
    /// Where the host is.
    pub state: HostStateKind,
    /// What kind of failure it is in; `None` unless it is failing.
    pub failure: FailureKind,
    /// Which layer the failure is in; meaningful only when `failure` is not
    /// `None`.
    pub layer: Layer,
    /// Whether it will be tried again by itself.
    pub retrying: bool,
    /// How many times it has failed since it was last connected, when it is
    /// reconnecting; zero otherwise.
    pub attempt: u32,
    /// Why an upgrade is on offer, for a connected host; `None` otherwise.
    pub upgrade: UpgradeKind,
    /// The version the host runs, as UTF-8, when an upgrade is on offer; null
    /// otherwise.
    pub installed_version: *const c_char,
    /// The version this build would put there, as UTF-8, when an upgrade is on
    /// offer; null otherwise.
    pub bundled_version: *const c_char,
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
    /// How wide a `Screen` is, in cells; zero otherwise.
    ///
    /// A screen is drawn at a size, and a surface reset to the wrong one puts
    /// every byte after it in the wrong cell — so the size travels with the
    /// bytes rather than being remembered from whenever the pane was last
    /// sized.
    pub columns: u16,
    /// How tall it is, in cells; zero otherwise.
    pub rows: u16,
    /// Which generation of the host's model a `Snapshot` is, or a `Delta`
    /// produces; zero otherwise.
    ///
    /// A change belongs to exactly one generation and the encoded change does
    /// not carry the number, so an application keeping a model of its own is
    /// given it here — it is what `iznik_protocol`'s `apply` wants beside the
    /// bytes, and what says whether anything was missed.
    pub generation: u64,
    /// This client's number for a command, on `CommandResult` and on a
    /// `Notification` that is about one; zero otherwise.
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
/// back into iznik: no call holds a lock of iznik's while it works, and none
/// is held while a callback runs.
///
/// Replacing it, or taking it away with a null, returns at once and waits for
/// nothing: a handler may be waiting for the very thread that asked. A call
/// already running may still be finishing; `iznik_wait_for_callbacks` is what
/// says it has, and so when the context given with it may be freed.
pub type EventCallback = Option<extern "C" fn(event: *const Event, context: *mut c_void)>;
