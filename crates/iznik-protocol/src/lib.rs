//! Framing, the `iznik/1` control messages, the session model, deltas and the reconciler: pure, dependency-free, golden-pinned.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod capabilities;
pub mod command;
pub mod delta;
pub mod dictionary;
pub mod frame;
pub mod identity;
pub mod message;
pub mod model;
pub mod reconcile;
