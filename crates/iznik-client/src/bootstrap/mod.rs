//! The bootstrap: probe, decide, upload, launch, handshake, snapshot, and the upgrade and uninstall paths.
//!
//! Filled by task `remote-launch` of plan 0005; until then this module holds only its documentation.

pub mod launch;
pub mod probe;
pub mod terminfo;
pub mod upload;
