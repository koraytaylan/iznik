//! The transport under a host: the system `ssh` with a control master, or a local daemon socket for the `unix:` alias.
//!
//! Filled by task `ssh-control-master` of plan 0005; until then this module holds only its documentation.

pub mod channel;
pub mod ssh;
