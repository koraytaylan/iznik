//! Pseudoterminal ownership: spawning the login shell in its own session and
//! turning its blocking descriptor into async streams.
//!
//! `spawn` opens the pseudoterminal and starts the program; `streams` — filled
//! by task `pty-streams` of plan 0002 — turns the blocking descriptor into
//! async output and input on dedicated threads.

pub mod spawn;
pub mod streams;
