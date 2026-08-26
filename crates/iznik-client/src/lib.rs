//! The client engine: SSH transport, bootstrap, the client-side model and reducer, optimistic commands and multi-host management.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod bootstrap;
pub mod commands;
pub mod host;
pub mod model;
pub mod reduce;
pub mod transport;
