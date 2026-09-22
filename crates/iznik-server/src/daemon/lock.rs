//! The exclusive non-blocking lock that enforces a single daemon instance and
//! names the holder.
//!
//! Single instance is enforced by the lock and never by the socket file: a
//! socket left behind by a killed daemon says nothing about whether one is
//! running, while a lock the kernel holds says exactly that. The file carries
//! the holder's process id so a second start can name it and `--stop` can
//! reach it, but the id is a courtesy — the lock is the truth.

use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use core::fmt::{self, Display, Formatter};

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::fcntl::{Flock, FlockArg};

#[cfg(windows)]
use super::socket;

/// The mode the lock file is created with: the owner's, and nobody else's.
const OWNER_ONLY: u32 = 0o600;

/// How many times a refused attempt is tried again before it is called a
/// holder. A daemon going, or a probe looking, holds this for microseconds.
const CONTENTION_ATTEMPTS: usize = 20;

/// How long between those attempts.
const CONTENTION_PAUSE: core::time::Duration = core::time::Duration::from_millis(5);

/// How old a Windows lock file must be, with nothing listening, before a new
/// daemon treats it as a killed predecessor. A start binds its port
/// immediately after creating the file, so a file younger than this is a
/// daemon still coming up, not a corpse.
#[cfg(windows)]
const STALE_LOCK: core::time::Duration = core::time::Duration::from_secs(2);

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
    /// The locked file. On Unix nothing reads it: what it is for is the lock
    /// the kernel holds, released when this is dropped. On Windows the file's
    /// existence is the lock, and this handle keeps that obvious.
    #[cfg(unix)]
    _held: Flock<File>,
    /// The locked file on Windows.
    #[cfg(windows)]
    _held: File,
    /// Where it is, so it can be removed when the daemon goes.
    path: PathBuf,
}

impl Lock {
    /// Takes the lock, or says who holds it.
    ///
    /// Asynchronous because a refusal is asked again after a pause, and this
    /// server does not block a worker thread to wait for anything.
    ///
    /// # Errors
    ///
    /// [`LockError::Held`] naming the holder when another daemon has it, and
    /// [`LockError::Io`] when the file cannot be opened or written.
    pub async fn acquire(path: &Path) -> Result<Lock, LockError> {
        #[cfg(unix)]
        {
            Self::acquire_exclusive(path).await
        }
        #[cfg(windows)]
        {
            acquire_windows(path).await
        }
    }

    /// Takes the lock on Unix.
    ///
    /// # Errors
    ///
    /// As [`Lock::acquire`].
    #[cfg(unix)]
    async fn acquire_exclusive(path: &Path) -> Result<Lock, LockError> {
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
        let mut patience = CONTENTION_ATTEMPTS;
        let held = loop {
            match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(held) => break held,
                // An interrupted attempt is not a refusal: it is asked again.
                Err((again, Errno::EINTR)) => file = again,
                // Nor is a moment's contention. A daemon on its way out, or a
                // `--stop` looking to see whether one is there, holds this for
                // microseconds; a start that gave up on the first refusal
                // would be losing a race rather than finding a holder.
                Err((again, Errno::EWOULDBLOCK)) if patience > 0 => {
                    patience = patience.saturating_sub(1);
                    tokio::time::sleep(CONTENTION_PAUSE).await;
                    file = again;
                }
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
        #[cfg(unix)]
        {
            let _removed = std::fs::remove_file(&self.path);
            drop(self);
        }
        #[cfg(windows)]
        {
            // A file this process still has open cannot be removed. Drop the
            // handle first. The file still exists until the removal, so another
            // start cannot create it in between.
            let path = self.path.clone();
            drop(self);
            let _removed = std::fs::remove_file(path);
        }
    }
}

/// Takes the lock on Windows by creating the file. A file that is already
/// there and whose port answers is a live daemon. A file older than
/// [`STALE_LOCK`] with nothing listening is a killed one and is removed.
///
/// # Errors
///
/// As [`Lock::acquire`].
#[cfg(windows)]
async fn acquire_windows(path: &Path) -> Result<Lock, LockError> {
    let mut patience = CONTENTION_ATTEMPTS;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(held) => {
                let lock = Lock {
                    _held: held,
                    path: path.to_path_buf(),
                };
                lock.record()?;
                return Ok(lock);
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                if patience == 0 || stale(path).await {
                    if stale(path).await {
                        let _removed = std::fs::remove_file(path);
                        let _removed = std::fs::remove_file(socket_beside(path));
                        continue;
                    }
                    return Err(LockError::Held {
                        process_id: holder(path).unwrap_or_default(),
                    });
                }
                patience = patience.saturating_sub(1);
                tokio::time::sleep(CONTENTION_PAUSE).await;
            }
            Err(source) => {
                return Err(LockError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
}

/// Whether `path` names a lock whose daemon is gone.
#[cfg(windows)]
async fn stale(path: &Path) -> bool {
    let elapsed = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| modified.elapsed())
        .is_ok_and(|elapsed| elapsed > STALE_LOCK);
    elapsed && !socket::answering(&socket_beside(path)).await
}

/// The endpoint file beside a lock file.
#[cfg(windows)]
fn socket_beside(path: &Path) -> PathBuf {
    path.with_file_name(socket::NAME)
}

/// What a look at the lock found.
#[derive(Debug)]
pub enum Holder {
    /// Nobody holds it, and the file may not even be there.
    Nobody,
    /// Somebody does.
    Held {
        /// The process id it records, or zero when the file said nothing.
        process_id: u32,
    },
    /// It could not be looked at, which is not the same as nobody holding it:
    /// a permission or a descriptor limit says nothing about who is running.
    Unknown {
        /// What the operating system said.
        source: std::io::Error,
    },
}

/// Who holds the lock, without taking it and without leaving anything behind:
/// it does not create the file, and it writes nothing into it.
///
/// [`Lock::acquire`] would do both, and a `--stop` that used it would put its
/// own process id in the file for the next reader — or for a second `--stop`,
/// which would then signal it.
#[must_use]
pub fn held_by(path: &Path) -> Holder {
    #[cfg(windows)]
    {
        return match std::fs::read_to_string(path) {
            Ok(text) => Holder::Held {
                process_id: text.trim().parse().unwrap_or_default(),
            },
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Holder::Nobody,
            Err(source) => Holder::Unknown { source },
        };
    }
    #[cfg(unix)]
    {
        held_exclusive(path)
    }
}

/// Who holds the lock on Unix, by trying a shared lock that a daemon's
/// exclusive hold refuses.
#[cfg(unix)]
fn held_exclusive(path: &Path) -> Holder {
    let opened = match OpenOptions::new().read(true).open(path) {
        Ok(opened) => opened,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Holder::Nobody,
        Err(source) => return Holder::Unknown { source },
    };
    // Shared, not exclusive: it still fails against a daemon's exclusive hold,
    // which is the question, and two probes at once do not refuse each other.
    match Flock::lock(opened, FlockArg::LockSharedNonblock) {
        // Taken and let go at once: nobody was holding it.
        Ok(_taken) => Holder::Nobody,
        Err((_file, _errno)) => Holder::Held {
            process_id: holder(path).unwrap_or_default(),
        },
    }
}

/// The process id a lock file records, if it records one this system could
/// have handed out.
#[must_use]
pub fn holder(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}
