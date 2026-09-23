//! Why the manager could not do something.

use std::path::PathBuf;

use iznik_protocol::message::MessageError;

use crate::bootstrap::launch::{BootstrapError, UpgradeError};
use crate::bootstrap::upload::UploadError;
use crate::host::identity::{AliasError, HostId};
use iznik_protocol::identity::PaneId;

/// Why the manager could not do something.
#[derive(Debug)]
pub enum ManagerError {
    /// Its runtime could not be built.
    Runtime {
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The artifacts could not be read from this machine.
    Artifacts {
        /// What went wrong.
        source: UploadError,
    },
    /// The name is not one a host may be held under.
    Alias {
        /// Why.
        source: AliasError,
    },
    /// No host of that name is held.
    UnknownHost {
        /// The name that was asked for.
        host: HostId,
    },
    /// The host's task has ended and is not taking orders.
    Gone {
        /// The host.
        host: HostId,
    },
    /// The log an application asked for will not be written.
    ///
    /// Refused rather than shrugged at: somebody who names a file wants what
    /// went wrong written to it, and the one moment they would find out it
    /// was never written is the moment they go looking for the reason
    /// something failed.
    Log {
        /// The file that was asked for.
        path: PathBuf,
        /// Why it will not be.
        detail: String,
    },
    /// The pane is not carrying anything just now.
    ///
    /// Its own refusal rather than an unknown host, which is what a held and
    /// connected host would otherwise be called: between a reconnection and
    /// the host re-announcing its panes, a pane can be left holding a number
    /// that now belongs to another, and credit for it would go where no pane
    /// would ever receive it. Nothing is wrong, and there is nothing to do
    /// but wait for the screen that says where it went.
    NotCarrying {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
    },
    /// A lock the manager holds was left broken by a panic under it.
    ///
    /// Its own error rather than an empty answer: a manager that reported no
    /// hosts, or an unknown one, would have whoever is watching believe
    /// something about the world instead of about this program.
    Poisoned {
        /// Which lock.
        what: &'static str,
    },
    /// The connected server cannot decode the command, so it was not sent.
    ///
    /// A server built before a command existed refuses its frame as garbage
    /// and ends the connection on it, so a client sends a command only when
    /// the server advertised it can decode it. Upgrading the host is what
    /// makes the command — and the feature that wants it — available.
    Unsupported {
        /// The host.
        host: HostId,
        /// What the command is called, for the words a person reads.
        command: &'static str,
    },
    /// The command is too large for one message, so it was not sent.
    Oversize {
        /// Why it could not be encoded.
        source: MessageError,
    },
    /// The host refused an upgrade, or could not be reached for one.
    Upgrade {
        /// What went wrong.
        source: UpgradeError,
    },
    /// The host could not be taken off.
    Uninstall {
        /// What went wrong.
        source: BootstrapError,
    },
}

impl core::fmt::Display for ManagerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ManagerError::Runtime { source } => {
                write!(formatter, "the manager's runtime: {source}")
            }
            ManagerError::Artifacts { source } => write!(formatter, "{source}"),
            ManagerError::Alias { source } => write!(formatter, "{source}"),
            ManagerError::Log { path, detail } => {
                write!(formatter, "the log at {}: {detail}", path.display())
            }
            ManagerError::NotCarrying { host, pane } => write!(
                formatter,
                "pane {} on {host} is not carrying anything just now",
                pane.0
            ),
            ManagerError::Poisoned { what } => {
                write!(formatter, "the manager's {what} was left broken by a panic")
            }
            ManagerError::UnknownHost { host } => write!(formatter, "{host} is not held"),
            ManagerError::Gone { host } => write!(formatter, "{host} is no longer running"),
            ManagerError::Unsupported { host, command } => write!(
                formatter,
                "{host} is running a server too old for {command}; upgrade the host to use it"
            ),
            ManagerError::Oversize { source } => {
                write!(formatter, "the command cannot be sent: {source}")
            }
            ManagerError::Upgrade { source } => write!(formatter, "{source}"),
            ManagerError::Uninstall { source } => write!(formatter, "{source}"),
        }
    }
}

impl core::error::Error for ManagerError {}
