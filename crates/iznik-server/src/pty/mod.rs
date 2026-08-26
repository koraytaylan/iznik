//! Pseudoterminal ownership: spawning the login shell in its own session and turning its blocking descriptor into async streams.
//!
//! Filled by task `pty-spawn` of plan 0002; until then this module holds only its documentation.

pub mod spawn;
pub mod streams;
