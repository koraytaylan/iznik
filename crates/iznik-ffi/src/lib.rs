//! The C ABI over `iznik-client`: the entry points the application calls, the error, the events, and the pane byte pipe.
#![doc = include_str!("../README.md")]

pub mod error;
pub mod model;
pub mod pane;
