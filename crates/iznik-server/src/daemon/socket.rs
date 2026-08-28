//! The unix socket the daemon listens on: a stale file removed and rebound
//! once the lock is held.
//!
//! Removing the file is safe only because the caller holds the lock — that is
//! what says no live daemon is listening on it. A socket file left by a killed
//! daemon would otherwise make every later start fail on `EADDRINUSE`, and
//! deciding by the file's existence instead would make a live daemon
//! indistinguishable from a dead one's leavings.

use std::path::{Path, PathBuf};

use core::fmt::{self, Display, Formatter};

use tokio::net::UnixListener;

/// Why the socket could not be bound.
#[derive(Debug)]
pub enum SocketError {
    /// The stale file could not be removed, or the socket could not be bound.
    Io {
        /// The socket path.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
}

impl Display for SocketError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SocketError::Io { path, source } => {
                write!(formatter, "the socket {}: {source}", path.display())
            }
        }
    }
}

impl core::error::Error for SocketError {}

/// Binds the socket, removing whatever a dead predecessor left there.
///
/// The caller must hold the lock: that, and not the file, is what says no
/// daemon is listening.
///
/// # Errors
///
/// [`SocketError::Io`] when the old file cannot be removed or the socket
/// cannot be bound.
pub fn bind(path: &Path) -> Result<UnixListener, SocketError> {
    if path.exists()
        && let Err(source) = std::fs::remove_file(path)
        && source.kind() != std::io::ErrorKind::NotFound
    {
        return Err(SocketError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    UnixListener::bind(path).map_err(|source| SocketError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Whether something is listening on the socket now. It connects rather than
/// looking for the file: a file that is there says nothing.
pub async fn answering(path: &Path) -> bool {
    tokio::net::UnixStream::connect(path).await.is_ok()
}
