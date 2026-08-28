//! What the manager says, turned into what the boundary hands over.
//!
//! One event of iznik's own becomes one of the application's: a kind, the host
//! it is about, the bytes it carries in `iznik/1`'s own encoding, and the
//! numbers that say which pane, which place in its stream, which generation
//! and which command. Nothing here touches a pointer — the crate root puts
//! what this decides into the C struct and makes the call.

use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_protocol::message::{ToClient, encode_to_client};

use crate::model::EventKind;

/// Everything one event carries, before it is put into the struct that
/// crosses.
pub(crate) struct Shaped {
    /// What it is about.
    pub kind: EventKind,
    /// The host, as the user's own alias.
    pub host: String,
    /// The bytes, whose meaning the kind decides.
    pub payload: Vec<u8>,
    /// The pane it names, or zero.
    pub pane: u64,
    /// Where in that pane's stream it sits, or zero.
    pub sequence: u64,
    /// How wide a screen is, or zero.
    pub columns: u16,
    /// How tall, or zero.
    pub rows: u16,
    /// Which generation a model event is, or zero.
    pub generation: u64,
    /// The command it answers, or zero.
    pub command: u64,
}

/// Everything one event carries, or nothing when it is not one to hand over.
pub(crate) fn shaped(event: &ManagerEvent) -> Option<Shaped> {
    let (kind, host, payload) = kind_of(event)?;
    Some(Shaped {
        kind,
        host,
        payload,
        pane: pane_of(event),
        sequence: sequence_of(event),
        columns: sized(event).0,
        rows: sized(event).1,
        generation: generation_of(event),
        command: command_of(event),
    })
}

/// What kind an event is, whose host it is about, and the bytes it carries.
fn kind_of(event: &ManagerEvent) -> Option<(EventKind, String, Vec<u8>)> {
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
        // A pane nobody attached to has stopped. Its own kind, with the pane
        // in the field for one: a state is something a person reads about a
        // host, and this is not about the host.
        ManagerEvent::Detached { host, .. } => {
            Some((EventKind::PaneDetached, host.0.clone(), Vec::new()))
        }
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
            // An answer that cannot be encoded is still an answer, and the
            // application is holding a number waiting for one: it is told in
            // words rather than left to wait for ever. The number is in
            // `command_id` either way.
            iznik_protocol::command::encode_command_outcome(outcome).map_or_else(
                |refusal| {
                    Some((
                        EventKind::Notification,
                        host.0.clone(),
                        format!("the host's answer could not be read: {refusal}").into_bytes(),
                    ))
                },
                |payload| Some((EventKind::CommandResult, host.0.clone(), payload)),
            )
        }
        Notification::Mark {
            host,
            pane,
            sequence,
            kind,
        } => {
            // As above: nothing that happened is dropped for want of an
            // encoding, and a mark nobody can read is still a mark.
            encode_to_client(&ToClient::Mark {
                pane: *pane,
                sequence: *sequence,
                kind: kind.clone(),
            })
            .map_or_else(
                |refusal| {
                    Some((
                        EventKind::Notification,
                        host.0.clone(),
                        format!("a mark could not be read: {refusal}").into_bytes(),
                    ))
                },
                |payload| Some((EventKind::Mark, host.0.clone(), payload)),
            )
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
        | ManagerEvent::Detached { pane, .. }
        | ManagerEvent::Notify(Notification::Mark { pane, .. }) => pane.0,
        _elsewhere => 0,
    }
}

/// The size a screen was drawn at, or nothing.
///
/// It travels with the bytes because a surface reset to the wrong size puts
/// every byte after it in the wrong cell, and the size a pane was last told
/// to be is not necessarily the size the screen in hand was drawn at.
fn sized(event: &ManagerEvent) -> (u16, u16) {
    match event {
        ManagerEvent::Screen { columns, rows, .. } => (*columns, *rows),
        _elsewhere => (0, 0),
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
