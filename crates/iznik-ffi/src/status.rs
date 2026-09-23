//! A host's state as a program reads it: the `iznik_host_status` a
//! `HostStatus` event points at, made from what the manager said.

use core::ffi::c_void;
use std::ffi::CString;

use iznik_client::bootstrap::launch::{Cause, Stage};
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::{HostState, UpgradeReason};

use crate::error::Layer;
use crate::model::{Event, EventKind, FailureKind, HostStateKind, HostStatus, UpgradeKind};

/// A status and the strings it points into, kept alive together for the call.
struct Held {
    /// The status.
    status: HostStatus,
    /// The versions an offer names, owned here.
    _versions: Option<(CString, CString)>,
}

/// The layer a bootstrap's stage belongs to, as a refusal from it reports.
fn staged(stage: Stage) -> Layer {
    match stage {
        Stage::Probe => Layer::Transport,
        Stage::Upload | Stage::Launch => Layer::Bootstrap,
        Stage::Handshake => Layer::Protocol,
    }
}

/// What kind of failure a cause is.
fn failure(cause: Cause) -> FailureKind {
    match cause {
        Cause::Transient => FailureKind::Transient,
        Cause::Credentials => FailureKind::Credentials,
        Cause::HostKey => FailureKind::HostKey,
        Cause::Unsupported => FailureKind::Unsupported,
    }
}

/// A status with nothing failing and nothing on offer.
fn plain(state: HostStateKind) -> HostStatus {
    HostStatus {
        state,
        failure: FailureKind::None,
        layer: Layer::Client,
        retrying: false,
        attempt: 0,
        upgrade: UpgradeKind::None,
        installed_version: core::ptr::null(),
        bundled_version: core::ptr::null(),
    }
}

/// A state as a status, with the strings it points into.
fn held(state: &HostState) -> Held {
    let mut versions = None;
    let status = match state {
        HostState::Disconnected => plain(HostStateKind::Disconnected),
        HostState::Probing => plain(HostStateKind::Probing),
        HostState::Bootstrapping { .. } => plain(HostStateKind::Bootstrapping),
        HostState::Connecting => plain(HostStateKind::Connecting),
        HostState::Upgrading => plain(HostStateKind::Upgrading),
        HostState::Connected { upgrade, .. } => {
            let mut status = plain(HostStateKind::Connected);
            if let Some(offer) = upgrade
                && let (Ok(installed), Ok(bundled)) = (
                    CString::new(offer.installed.crate_version.clone()),
                    CString::new(offer.bundled.crate_version.clone()),
                )
            {
                status.upgrade = match offer.reason {
                    UpgradeReason::Version => UpgradeKind::Version,
                    UpgradeReason::Capabilities => UpgradeKind::Capabilities,
                };
                status.installed_version = installed.as_ptr();
                status.bundled_version = bundled.as_ptr();
                versions = Some((installed, bundled));
            }
            status
        }
        HostState::Reconnecting { attempt, .. } => HostStatus {
            failure: FailureKind::Transient,
            layer: Layer::Transport,
            retrying: true,
            attempt: *attempt,
            ..plain(HostStateKind::Reconnecting)
        },
        HostState::Failed {
            cause,
            stage,
            retry_at,
            ..
        } => HostStatus {
            failure: failure(*cause),
            layer: stage.map_or(Layer::Client, staged),
            retrying: retry_at.is_some(),
            ..plain(HostStateKind::Failed)
        },
    };
    Held {
        status,
        _versions: versions,
    }
}

/// Whether an event is a host's move, which a status follows.
pub(crate) fn is_a_move(event: &ManagerEvent) -> bool {
    matches!(
        event,
        ManagerEvent::Moved { .. } | ManagerEvent::Removed { .. }
    )
}

/// Hands the application the status a move carries, when the event is a
/// move.
pub(crate) fn carry_status(
    callback: extern "C" fn(*const Event, *mut c_void),
    context: *mut c_void,
    event: &ManagerEvent,
) {
    let (kept, named) = match event {
        ManagerEvent::Moved { state, host } => (held(state), host),
        ManagerEvent::Removed { host } => (
            Held {
                status: plain(HostStateKind::Removed),
                _versions: None,
            },
            host,
        ),
        _elsewhere => return,
    };
    let Ok(host) = CString::new(named.0.clone()) else {
        return;
    };
    let told = Event {
        kind: EventKind::HostStatus,
        host: host.as_ptr(),
        pane: 0,
        sequence: 0,
        columns: 0,
        rows: 0,
        generation: 0,
        command_id: 0,
        payload: (&raw const kept.status).cast::<u8>(),
        payload_length: size_of::<HostStatus>(),
        answered_length: 0,
        stream: 0,
    };
    callback(&raw const told, context);
}
