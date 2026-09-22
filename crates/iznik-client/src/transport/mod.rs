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

#[cfg(unix)]
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
#[cfg(unix)]
const OWNER_ONLY: u32 = 0o700;

/// How many bytes of a unix socket path the platform allows, on the one that
/// allows the least: macOS, where `sockaddr_un.sun_path` is 104 bytes
/// including its terminator.
const SOCKET_PATH_LIMIT: usize = 104;

/// The byte that ends the path inside that array, which is part of the 104.
const SOCKET_PATH_TERMINATOR: usize = 1;

/// How many bytes `ssh` itself reserves when it binds: it listens on
/// `<ControlPath>.<sixteen random characters>` and renames that into place, so
/// a configured path must leave room for a `.` and sixteen bytes or the bind
/// fails with "too long for Unix domain socket" whatever the platform allows.
///
/// OpenSSH's `mux.c` makes this suffix sixteen characters and one separator.
const CONTROL_TEMPORARY_BYTES: usize = 1 + 16;

/// The most bytes a configured control path may take, on the platform that
/// allows the least: the array, less its terminator, less what `ssh` reserves
/// for the bind it renames into place.
pub const MAXIMUM_CONTROL_PATH_BYTES: usize =
    SOCKET_PATH_LIMIT - SOCKET_PATH_TERMINATOR - CONTROL_TEMPORARY_BYTES;

/// How many hexadecimal characters of an alias's digest name its control
/// socket, which is the most that will fit: an alias itself can exceed a
/// socket path and a `ProxyJump` chain routinely does, and a digest never can.
const CONTROL_NAME_LENGTH: usize = 16;

/// The fewest characters a control name may be shortened to when its
/// directory is deep enough that the full digest would not fit: eight
/// hexadecimal characters of `SHA-256`, thirty-two bits.
///
/// It is a floor rather than something to go below because two aliases that
/// shared a control path would share an open connection to a host, and the
/// second of them would run its command on the first's host. A directory with
/// no room at all gets this much anyway and cannot bind: `ssh` says the path
/// is too long and names it, which is the failure a person should see, and it
/// is not one whose answer is a name short enough to collide.
pub const MINIMUM_CONTROL_NAME_LENGTH: usize = 8;

/// The byte a path joins its components with, on the machines this runs on.
const PATH_SEPARATOR: usize = 1;

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
    /// What is there is not a directory this user owns, so it is not one to
    /// put an open connection to a host inside.
    NotOurs {
        /// What was found.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
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
            PathsError::NotOurs { path, detail } => write!(
                formatter,
                "{} cannot hold this client's connections: {detail}",
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
            // The fallback base is a predictable name in a shared temporary
            // directory, and `create_dir_all` follows a symbolic link. Someone
            // who plants one there first would otherwise have this restrict a
            // directory of their choosing and then fill it with live SSH
            // connections. What is trusted is a real directory this user owns.
            restrict(made)?;
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

    /// Where the control socket for `alias` belongs: a digest of the alias, not
    /// the alias itself, because a socket path has a length limit that an alias
    /// does not.
    ///
    /// The digest is only as long as the directory leaves room for, so the path
    /// is under the platform's limit whatever base it was resolved under.
    #[must_use]
    pub fn control_path(&self, alias: &str) -> PathBuf {
        self.control_directory
            .join(control_name(alias, self.control_room()))
    }

    /// How many bytes a control name under this directory may take: the most a
    /// configured control path may be, less the directory and the byte that
    /// joins the name to it.
    ///
    /// Saturating rather than refusing: a directory with no room is a control
    /// socket that cannot be bound, and what says so is `ssh` refusing that
    /// path by name, not arithmetic that would have panicked first.
    fn control_room(&self) -> usize {
        let taken = self
            .control_directory
            .as_os_str()
            .len()
            .saturating_add(PATH_SEPARATOR);
        MAXIMUM_CONTROL_PATH_BYTES.saturating_sub(taken)
    }
}

/// Refuses a link or a non-directory, then on Unix restricts the mode to the owner.
///
/// # Errors
///
/// [`PathsError::Io`] when the path cannot be read or its mode cannot be set,
/// and [`PathsError::NotOurs`] when it is not a directory this user owns.
fn restrict(path: &Path) -> Result<(), PathsError> {
    own_directory(path)?;
    #[cfg(unix)]
    std::fs::set_permissions(
        path,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(OWNER_ONLY),
    )
    .map_err(|source| PathsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Refuses anything at `path` that is not a directory this user owns, without
/// following a link to find out. On Windows the profile directory's own
/// access control is what keeps it private, so only the directory check runs.
///
/// # Errors
///
/// [`PathsError::Io`] when it cannot be read, and [`PathsError::NotOurs`] when
/// it is a link, not a directory, or somebody else's.
fn own_directory(path: &Path) -> Result<(), PathsError> {
    let held = std::fs::symlink_metadata(path).map_err(|source| PathsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !held.is_dir() {
        return Err(PathsError::NotOurs {
            path: path.to_path_buf(),
            detail: "it is not a directory".to_owned(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let ours = Uid::current().as_raw();
        if held.uid() != ours {
            return Err(PathsError::NotOurs {
                path: path.to_path_buf(),
                detail: format!("it belongs to {} and not to {ours}", held.uid()),
            });
        }
    }
    Ok(())
}

/// The directory the client's runtime files belong in on this machine.
fn base() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|held| !held.is_empty()) {
            return PathBuf::from(runtime).join(DIRECTORY_NAME);
        }
        let temporary = std::env::var_os("TMPDIR")
            .filter(|held| !held.is_empty())
            .map_or_else(std::env::temp_dir, PathBuf::from);
        temporary.join(format!("{DIRECTORY_NAME}-{}", Uid::current().as_raw()))
    }
    #[cfg(not(unix))]
    {
        if let Some(profile) = std::env::var_os("LOCALAPPDATA").filter(|held| !held.is_empty()) {
            return PathBuf::from(profile).join(DIRECTORY_NAME);
        }
        let temporary = std::env::var_os("TEMP")
            .filter(|held| !held.is_empty())
            .map_or_else(std::env::temp_dir, PathBuf::from);
        let account = std::env::var("USERNAME").unwrap_or_default();
        let account = if account.is_empty() {
            "user".to_owned()
        } else {
            account
        };
        temporary.join(format!("{DIRECTORY_NAME}-{account}"))
    }
}

/// The file name a control socket for `alias` takes: at most the sixteen
/// characters of the alias's digest it fits in, and no fewer than eight.
///
/// The room shrinks with the directory the name is going into, so that a
/// machine whose temporary directory is long still binds a socket shorter than
/// the platform's limit. A directory so deep that even eight characters do not
/// fit gets those eight anyway: `ssh` then refuses it by name, which is a
/// legible failure, where arithmetic here would be a panic.
#[must_use]
pub fn control_name(alias: &str, room: usize) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(alias.as_bytes());
    let name: String = digest
        .iter()
        .fold(String::new(), |mut held, byte| {
            use core::fmt::Write as _;
            // A digit that will not format is not a thing that happens.
            let _written = write!(held, "{byte:02x}");
            held
        })
        .chars()
        .take(room.clamp(MINIMUM_CONTROL_NAME_LENGTH, CONTROL_NAME_LENGTH))
        .collect();
    name
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
