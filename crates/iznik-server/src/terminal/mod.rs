//! The emulator every pane's bytes are fed into, and what the server reads back
//! out of it.
//!
//! `mirror` is the `libghostty-vt` terminal — the engine the client renders with
//! — and the single thread every pane's `!Send` emulator and task lives on;
//! `marks` recognizes the shell-integration events (prompts, directories, titles
//! and alternate-screen switches) as the bytes pass; `screen` serializes the
//! emulator's screen so a cold attach is byte-exact.

pub mod marks;
pub mod mirror;
pub mod screen;
