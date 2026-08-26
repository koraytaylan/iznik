//! The golden loader, the headless VT oracle, the pseudoterminal harness, the fidelity corpus, the model generator, the protocol test client and the in-process stack.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod client;
pub mod corpus;
pub mod generate;
pub mod golden;
pub mod metrics;
pub mod pty;
pub mod stack;
pub mod vt;
