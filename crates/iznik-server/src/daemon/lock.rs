//! The exclusive non-blocking lock that enforces a single daemon instance and
//! names the holder.
//!
//! Single instance is enforced by the lock and never by the socket file: a
//! socket left behind by a killed daemon says nothing about whether one is
//! running, while a lock the kernel holds says exactly that. The file carries
//! the holder's process id so a second start can name it and `--stop` can
//! reach it, but the id is a courtesy — the lock is the truth.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use core::fmt::{self, Display, Formatter};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

/// The mode the lock file is created with: the owner's, and nobody else's.
const OWNER_ONLY: u32 = 0o600;

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another daemon holds it.
    Held {
        /// The process id it recorded, or zero when the file said nothing.
        process_id: u32,
    },
    /// The file could not be opened, written or read.
    Io {
        /// The lock file.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
}

impl Display for LockError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Held { process_id } => write!(
                formatter,
                "another iznik-server holds the lock (process {process_id})"
            ),
            LockError::Io { path, source } => {
                write!(formatter, "the lock file {}: {source}", path.display())
            }
        }
    }
}

impl core::error::Error for LockError {}

/// An exclusive lock on the daemon's lock file, released when it is dropped
/// and removed by [`Lock::release`].
#[derive(Debug)]
pub struct Lock {
    /// The locked file. Nothing reads it: what it is for is the lock the
    /// kernel holds on it, which is released when it is dropped.
    _held: Flock<File>,
    /// Where it is, so it can be removed when the daemon goes.
    path: PathBuf,
}

impl Lock {
    /// Takes the lock, or says who holds it.
    ///
    /// # Errors
    ///
    /// [`LockError::Held`] naming the holder when another daemon has it, and
    /// [`LockError::Io`] when the file cannot be opened or written.
    pub fn acquire(path: &Path) -> Result<Lock, LockError> {
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(OWNER_ONLY)
            .open(path)
            .map_err(|source| LockError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut file = opened;
        let held = loop {
            match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(held) => break held,
                // An interrupted attempt is not a refusal: it is asked again.
                Err((again, Errno::EINTR)) => file = again,
                Err((_file, Errno::EWOULDBLOCK)) => {
                    return Err(LockError::Held {
                        process_id: holder(path).unwrap_or_default(),
                    });
                }
                Err((_file, errno)) => {
                    return Err(LockError::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::from(errno),
                    });
                }
            }
        };
        let lock = Lock {
            _held: held,
            path: path.to_path_buf(),
        };
        lock.record()?;
        Ok(lock)
    }

    /// Writes this process's id into the locked file, replacing whatever a
    /// dead predecessor left. It writes through the path rather than through
    /// the handle it holds: the lock is on the open file this holds, and a
    /// truncating write through the same path does not disturb it.
    ///
    /// # Errors
    ///
    /// [`LockError::Io`] when the file cannot be written.
    fn record(&self) -> Result<(), LockError> {
        std::fs::write(&self.path, format!("{}\n", std::process::id())).map_err(|source| {
            LockError::Io {
                path: self.path.clone(),
                source,
            }
        })
    }

    /// Where the lock file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the file and then releases the lock, which is what a daemon
    /// does on its way out. In that order: unlocking first would let the next
    /// daemon take the lock on this same file, and the removal would then take
    /// its lock file out from under it and let a third daemon lock a fresh one
    /// — two daemons at once, which is the one thing this exists to prevent.
    pub fn release(self) {
        let _removed = std::fs::remove_file(&self.path);
        drop(self);
    }
}

/// Who holds the lock, without taking it and without leaving anything behind:
/// it does not create the file, and it writes nothing into it.
///
/// [`Lock::acquire`] would do both, and a `--stop` that used it would put its
/// own process id in the file for the next reader — or for a second `--stop`,
/// which would then signal it.
#[must_use]
pub fn held_by(path: &Path) -> Option<u32> {
    let opened = OpenOptions::new().read(true).open(path).ok()?;
    match Flock::lock(opened, FlockArg::LockExclusiveNonblock) {
        // Taken and let go at once: nobody was holding it.
        Ok(_taken) => None,
        Err((_file, _errno)) => Some(holder(path).unwrap_or_default()),
    }
}

/// The process id a lock file records, if it records one this system could
/// have handed out.
#[must_use]
pub fn holder(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}
