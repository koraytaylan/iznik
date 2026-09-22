//! The endpoint the daemon listens on.
//!
//! On Unix that is a socket file: a stale file removed and rebound once the
//! lock is held. Removing the file is safe only because the caller holds the
//! lock — that is what says no live daemon is listening on it.
//!
//! On Windows it is a loopback TCP port. The path is a file holding
//! `127.0.0.1:<port>`. The port is released when the process ends, so a killed
//! daemon cannot leave a listener behind the way a socket file can.

#[cfg(windows)]
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use core::fmt::{self, Display, Formatter};

/// The file name of the endpoint, beside the lock.
pub const NAME: &str = "server.sock";

/// A connected client.
#[cfg(unix)]
pub type Stream = tokio::net::UnixStream;
/// A connected client.
#[cfg(windows)]
pub type Stream = tokio::net::TcpStream;

/// The listener the accept loop holds.
#[cfg(unix)]
pub type Listener = tokio::net::UnixListener;
/// The listener the accept loop holds.
#[cfg(windows)]
pub type Listener = tokio::net::TcpListener;

/// Why the endpoint could not be bound.
#[derive(Debug)]
pub enum SocketError {
    /// The stale file could not be removed, or the endpoint could not be bound.
    Io {
        /// The path.
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

/// Binds the endpoint, removing whatever a dead predecessor left at `path`.
///
/// The caller must hold the lock.
///
/// # Errors
///
/// [`SocketError::Io`] when the old file cannot be removed or the endpoint
/// cannot be bound.
pub fn bind(path: &Path) -> Result<Listener, SocketError> {
    if path.exists()
        && let Err(source) = std::fs::remove_file(path)
        && source.kind() != std::io::ErrorKind::NotFound
    {
        return Err(SocketError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    #[cfg(unix)]
    {
        tokio::net::UnixListener::bind(path).map_err(|source| SocketError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
    #[cfg(windows)]
    {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|source| {
            SocketError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|source| SocketError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let address = listener.local_addr().map_err(|source| SocketError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::write(path, address.to_string()).map_err(|source| SocketError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        tokio::net::TcpListener::from_std(listener).map_err(|source| SocketError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Connects to whatever [`bind`] published at `path`.
///
/// # Errors
///
/// When the file is missing, unreadable, or nothing is listening.
pub async fn connect(path: &Path) -> std::io::Result<Stream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(path).await
    }
    #[cfg(windows)]
    {
        let address = std::fs::read_to_string(path)?;
        tokio::net::TcpStream::connect(address.trim()).await
    }
}

/// Whether something is listening now. It connects rather than looking for the
/// file: a file that is there says nothing.
pub async fn answering(path: &Path) -> bool {
    connect(path).await.is_ok()
}
