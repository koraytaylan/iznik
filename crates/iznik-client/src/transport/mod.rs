//! The transport under a host: the system `ssh` with a control master, or a local daemon socket for the `unix:` alias.
//!
//! Two rules decide this module. The first is that iznik never reimplements
//! something `ssh` already does: a user's `~/.ssh/config` is where their
//! `ProxyJump` chain, their `Match` blocks, their bastion, their hardware
//! token and their organization's certificates already live, and the way to
//! honor all of it is to hand the alias to `ssh` and pass none of the options
//! a person could have configured. The second is that one alias form is
//! iznik's own: `unix:<path>` names a daemon socket on this machine and is
//! reached with no SSH at all, which is what every in-process test, the
//! plumbing commands and the C smoke program use.

pub mod channel;
pub mod ssh;

use core::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use nix::unistd::Uid;

use crate::transport::ssh::{SshOptions, SshTransport};

/// The prefix that names a daemon socket on this machine rather than a host
/// `ssh` would reach.
pub const LOCAL_PREFIX: &str = "unix:";

/// The directory the client's own runtime files belong in, under whichever
/// base this machine offers.
const DIRECTORY_NAME: &str = "iznik-client";

/// The directory control sockets are named in, under that one.
const CONTROL_DIRECTORY_NAME: &str = "control";

/// The log the client writes, under that one.
const LOG_NAME: &str = "client.log";

/// Owner-only, because a control socket is an open connection to a host.
const OWNER_ONLY: u32 = 0o700;

/// How many hexadecimal characters of an alias's digest name its control
/// socket.
///
/// A unix socket path is limited to 104 bytes on macOS, which the alias
/// itself can exceed and a `ProxyJump` chain routinely does; sixteen
/// characters of `SHA-256` is short enough to fit under any runtime directory
/// and long enough that two aliases will not collide.
const CONTROL_NAME_LENGTH: usize = 16;

/// Where the client keeps what belongs to this machine rather than to a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientRuntimePaths {
    /// The directory holding the rest.
    pub directory: PathBuf,
    /// Where control sockets are named.
    pub control_directory: PathBuf,
    /// The log file.
    pub log: PathBuf,
}

/// Why the client's runtime paths could not be settled.
#[derive(Debug)]
pub enum PathsError {
    /// A directory could not be created or its mode could not be set.
    Io {
        /// The directory.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
}

impl Display for PathsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            PathsError::Io { path, source } => write!(
                formatter,
                "the client's runtime directory {}: {source}",
                path.display()
            ),
        }
    }
}

impl core::error::Error for PathsError {}

impl ClientRuntimePaths {
    /// The paths under `directory`, whose control directory is created with
    /// mode `0700` if it is not there.
    ///
    /// # Errors
    ///
    /// [`PathsError::Io`] when a directory cannot be created or restricted.
    pub fn under(directory: &Path) -> Result<ClientRuntimePaths, PathsError> {
        let control_directory = directory.join(CONTROL_DIRECTORY_NAME);
        for made in [directory, &control_directory] {
            std::fs::create_dir_all(made).map_err(|source| PathsError::Io {
                path: made.to_path_buf(),
                source,
            })?;
            std::fs::set_permissions(
                made,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(OWNER_ONLY),
            )
            .map_err(|source| PathsError::Io {
                path: made.to_path_buf(),
                source,
            })?;
        }
        Ok(ClientRuntimePaths {
            log: directory.join(LOG_NAME),
            control_directory,
            directory: directory.to_path_buf(),
        })
    }

    /// The paths this machine allows: under `XDG_RUNTIME_DIR` when it is set,
    /// else under `TMPDIR`, else under the system's temporary directory, each
    /// in a directory of this user's own.
    ///
    /// # Errors
    ///
    /// As [`ClientRuntimePaths::under`].
    pub fn resolve() -> Result<ClientRuntimePaths, PathsError> {
        ClientRuntimePaths::under(&base())
    }

    /// Where the control socket for `alias` belongs: a short digest of the
    /// alias, not the alias itself, because a socket path has a length limit
    /// that an alias does not.
    #[must_use]
    pub fn control_path(&self, alias: &str) -> PathBuf {
        self.control_directory.join(control_name(alias))
    }
}

/// The directory the client's runtime files belong in on this machine.
fn base() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|held| !held.is_empty()) {
        return PathBuf::from(runtime).join(DIRECTORY_NAME);
    }
    let temporary = std::env::var_os("TMPDIR")
        .filter(|held| !held.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from);
    temporary.join(format!("{DIRECTORY_NAME}-{}", Uid::current().as_raw()))
}

/// The file name a control socket for `alias` takes.
#[must_use]
pub fn control_name(alias: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(alias.as_bytes());
    digest
        .iter()
        .fold(String::new(), |mut held, byte| {
            use core::fmt::Write as _;
            // A digit that will not format is not a thing that happens.
            let _written = write!(held, "{byte:02x}");
            held
        })
        .chars()
        .take(CONTROL_NAME_LENGTH)
        .collect()
}

/// How a host is reached.
#[derive(Clone, Debug)]
pub enum Transport {
    /// Through the system `ssh`.
    Ssh(SshTransport),
    /// Straight to a daemon socket on this machine. There is no relay to
    /// start: whatever uses this alias owns the daemon behind it.
    Local {
        /// The socket.
        socket: PathBuf,
    },
}

impl Transport {
    /// The transport an alias names: `unix:<path>` is local, and everything
    /// else is handed to `ssh` untouched.
    #[must_use]
    pub fn for_alias(alias: &str, paths: &ClientRuntimePaths, options: SshOptions) -> Transport {
        match alias.strip_prefix(LOCAL_PREFIX) {
            Some(socket) => Transport::Local {
                socket: PathBuf::from(socket),
            },
            None => Transport::Ssh(SshTransport::new(alias, paths, options)),
        }
    }

    /// The alias this was made for, which is what a person typed and what an
    /// error must name.
    #[must_use]
    pub fn alias(&self) -> String {
        match self {
            Transport::Ssh(transport) => transport.alias().to_owned(),
            Transport::Local { socket } => format!("{LOCAL_PREFIX}{}", socket.display()),
        }
    }
}
