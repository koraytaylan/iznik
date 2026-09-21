//! The session registry: the authoritative host model, the panes behind it,
//! and the numbered deltas every change to it emits.
//!
//! `registry` holds the model and the operations that change it; `commands`
//! turns a client's `SessionCommand` into one of those operations and answers
//! it exactly once. [`RegistryError`] is here rather than in either because
//! `commands` maps it to the rejection code a client is answered with, and it
//! is the vocabulary both share for what an operation refused.
//!
//! Bytes are not a session. What a client needs to know is which panes exist,
//! how a tab arranges them, what each is called and where its shell is — and
//! it needs that to stay true across a reconnect and across a second client on
//! another machine. That is what this holds, and every change to it is a
//! delta a client applies rather than a state it re-reads.

pub mod commands;
pub mod registry;

use iznik_protocol::identity::{PaneId, SessionId, TabId};
use iznik_protocol::model::ModelError;

use crate::pane::PaneError;

/// Why an operation could not be carried out.
#[derive(Debug)]
pub enum RegistryError {
    /// The host holds no such session.
    UnknownSession {
        /// The session named.
        session: SessionId,
    },
    /// The host holds no such tab.
    UnknownTab {
        /// The tab named.
        tab: TabId,
    },
    /// The host holds no such pane.
    UnknownPane {
        /// The pane named.
        pane: PaneId,
    },
    /// A name was empty, which no session or tab may carry.
    EmptyName,
    /// An order was not a permutation of the session's tabs.
    NotAPermutation {
        /// The session whose tabs they are.
        session: SessionId,
    },
    /// An order was not a permutation of the host's sessions.
    NotASessionPermutation,
    /// A layout does not place exactly the tab's panes, each once.
    InvalidLayout {
        /// The tab it was for.
        tab: TabId,
        /// What is wrong with it.
        error: ModelError,
    },
    /// A pseudoterminal or its child could not be started.
    Spawn(PaneError),
    /// A change the registry's own reconciler refused — a bug above. What was
    /// started is taken away again, so nothing was made.
    Refused {
        /// What was being done.
        detail: String,
    },
}

impl core::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RegistryError::UnknownSession { session } => {
                write!(formatter, "the host holds no session {}", session.0)
            }
            RegistryError::UnknownTab { tab } => {
                write!(formatter, "the host holds no tab {}", tab.0)
            }
            RegistryError::UnknownPane { pane } => {
                write!(formatter, "the host holds no pane {}", pane.0)
            }
            RegistryError::EmptyName => write!(formatter, "a name may not be empty"),
            RegistryError::NotAPermutation { session } => write!(
                formatter,
                "the order given is not a permutation of session {}'s tabs",
                session.0
            ),
            RegistryError::NotASessionPermutation => write!(
                formatter,
                "the order given is not a permutation of the host's sessions"
            ),
            RegistryError::InvalidLayout { tab, error } => {
                write!(formatter, "the layout for tab {}: {error}", tab.0)
            }
            RegistryError::Spawn(error) => write!(formatter, "{error}"),
            RegistryError::Refused { detail } => {
                write!(formatter, "the registry could not {detail}")
            }
        }
    }
}

impl core::error::Error for RegistryError {}

impl From<PaneError> for RegistryError {
    fn from(error: PaneError) -> RegistryError {
        RegistryError::Spawn(error)
    }
}
