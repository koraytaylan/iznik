//! The session registry: the authoritative host model, the panes behind it,
//! and the numbered deltas every change to it emits.
//!
//! `registry` holds the model and the operations that change it; `commands`
//! turns a client's `SessionCommand` into one of those operations and answers
//! it exactly once.
//!
//! Bytes are not a session. What a client needs to know is which panes exist,
//! how a tab arranges them, what each is called and where its shell is — and
//! it needs that to stay true across a reconnect and across a second client on
//! another machine. That is what this holds, and every change to it is a
//! delta a client applies rather than a state it re-reads.

pub mod commands;
pub mod registry;
