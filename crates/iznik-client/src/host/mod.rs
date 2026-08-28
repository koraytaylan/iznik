//! Several hosts at once: what one is called, where it is in its life, and
//! the manager that runs a task for each.
//!
//! [`identity`] names them — a host by the alias its user typed, a pane by
//! `iznik://<host>/<pane>` — and [`state`] decides what happens to one when a
//! link dies, as a table rather than as a scattering of conditions inside a
//! loop. [`manager`] is filled by task `connection-manager` of plan 0005;
//! until then that module holds only its documentation.

pub mod identity;
pub mod manager;
pub mod state;
