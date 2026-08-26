//! One multiplexer per client connection: the pump that carries every subscribed pane over one link under credit windows.
//!
//! Filled by task `multiplexer-assembly` of plan 0003; until then this module holds only its documentation.

pub mod channel;
pub mod credit;
pub mod scheduler;
