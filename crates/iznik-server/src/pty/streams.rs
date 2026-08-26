//! The one module where blocking I/O exists: dedicated threads that turn the pseudoterminal descriptor into an async output stream and an input queue.
//!
//! Filled by task `pty-streams` of plan 0002; until then this module holds only its documentation.
