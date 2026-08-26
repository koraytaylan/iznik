//! Multi-host: host identity, the per-host connection state machine, and the manager that runs one task per host.
//!
//! Filled by task `host-identity-and-state` of plan 0005; until then this module holds only its documentation.

pub mod identity;
pub mod manager;
pub mod state;
