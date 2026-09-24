//! Where a daemon's files live — the runtime directory and the socket, lock,
//! log and agent link in it — and the terminfo the bootstrap installed beside
//! its binary.

use std::path::{Path, PathBuf};

use core::fmt::{self, Display, Formatter};

#[cfg(unix)]
use iznik_protocol::identity::BuildDigest;
use nix::unistd::Uid;

use super::{agent, socket};

/// The directory the runtime files live in, under whichever base is allowed.
const DIRECTORY_NAME: &str = "iznik";

/// The socket every client connects to, inside that directory.
const SOCKET_NAME: &str = socket::NAME;

/// The lock that enforces a single instance, beside it.
const LOCK_NAME: &str = "server.lock";

/// The log, beside both.
const LOG_NAME: &str = "server.log";

/// The mode the runtime directory is created with: the owner's, and nobody
/// else's — a socket anyone can connect to is a shell anyone can have.
#[cfg(unix)]
const OWNER_ONLY: u32 = 0o700;

/// Where the daemon's files live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePaths {
    /// The directory holding all three.
    pub directory: PathBuf,
    /// The socket clients connect to.
    pub socket: PathBuf,
    /// The file whose lock enforces a single instance.
    pub lock: PathBuf,
    /// The log file.
    pub log: PathBuf,
    /// The link every pane's `SSH_AUTH_SOCK` names, which each relay points
    /// at the agent of its own connection.
    pub agent: PathBuf,
}

/// Why the runtime paths could not be settled.
#[derive(Debug)]
pub enum PathsError {
    /// The directory could not be created or its mode could not be set.
    Io {
        /// The directory.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// Something is at the path that is not a directory this user owns — a
    /// link, a file, or somebody else's directory — and it is not used.
    NotOurs {
        /// The path.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },
}

impl Display for PathsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            PathsError::Io { path, source } => {
                write!(
                    formatter,
                    "the runtime directory {}: {source}",
                    path.display()
                )
            }
            PathsError::NotOurs { path, detail } => write!(
                formatter,
                "the runtime directory {} is not this user's own: {detail}",
                path.display()
            ),
        }
    }
}

impl core::error::Error for PathsError {}

impl RuntimePaths {
    /// The paths under `directory`, which is created with mode `0700` if it is
    /// not there.
    ///
    /// The fallback base is a predictable name in a shared temporary
    /// directory, and `create_dir_all` follows a symbolic link: someone who
    /// planted one there first would have the daemon restrict a directory of
    /// their choosing and put its socket — a shell for whoever connects — and
    /// its log in it. So what is there is looked at without following a link,
    /// and used only when it is a real directory this user owns.
    ///
    /// # Errors
    ///
    /// [`PathsError::Io`] when the directory cannot be created or restricted,
    /// and [`PathsError::NotOurs`] when what is there is a link, not a
    /// directory, or somebody else's.
    pub fn under(directory: &Path) -> Result<RuntimePaths, PathsError> {
        std::fs::create_dir_all(directory).map_err(|source| PathsError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        own_directory(directory)?;
        #[cfg(unix)]
        std::fs::set_permissions(
            directory,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(OWNER_ONLY),
        )
        .map_err(|source| PathsError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        Ok(RuntimePaths {
            socket: directory.join(SOCKET_NAME),
            lock: directory.join(LOCK_NAME),
            log: directory.join(LOG_NAME),
            agent: directory.join(agent::AGENT_NAME),
            directory: directory.to_path_buf(),
        })
    }

    /// The paths this host allows: under `XDG_RUNTIME_DIR` when it is set,
    /// else under `TMPDIR`, else under `/tmp`, each in a directory of this
    /// user's own.
    ///
    /// A locked-down host with no runtime directory is a real case, and one
    /// that must not be discovered in the middle of somebody's first bootstrap.
    ///
    /// # Errors
    ///
    /// As [`RuntimePaths::under`].
    pub fn resolve() -> Result<RuntimePaths, PathsError> {
        RuntimePaths::under(&base())
    }
}

/// Refuses anything at `path` that is not a directory this user owns, without
/// following a link to find out. On Windows the profile directory's own access
/// control is what keeps it private, so only the directory check runs.
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

/// The directory the runtime files belong in on this host.
fn base() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("LOCALAPPDATA").filter(|held| !held.is_empty()) {
            return PathBuf::from(profile).join(DIRECTORY_NAME);
        }
        let temporary = std::env::var_os("TEMP")
            .filter(|held| !held.is_empty())
            .map_or_else(std::env::temp_dir, PathBuf::from);
        return temporary.join(DIRECTORY_NAME);
    }
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
}

/// The directory under an install prefix the bootstrap compiles the
/// terminfo into.
const TERMINFO_DIRECTORY: &str = "terminfo";

/// The entry a pane's ghostty `TERM` needs.
const TERMINFO_ENTRY: &str = "xterm-ghostty";

/// The terminfo the bootstrap installed beside this binary, if it did: the
/// server is `<prefix>/bin/iznik-server` and the terminfo `<prefix>/terminfo`,
/// compiled there by the host's own `tic` into a subdirectory named for the
/// entry's first letter — `x` on most hosts, its hexadecimal code `78` on
/// macOS — so any subdirectory holding the entry will do. A binary run from
/// anywhere else, or on a host with no `tic`, has none, and its panes are
/// given the fallback `TERM`.
#[must_use]
pub fn terminfo_beside(executable: &Path) -> Option<PathBuf> {
    let prefix = executable.parent()?.parent()?;
    let directory = prefix.join(TERMINFO_DIRECTORY);
    let compiled = std::fs::read_dir(&directory)
        .ok()?
        .filter_map(Result::ok)
        .any(|entry| entry.path().join(TERMINFO_ENTRY).is_file());
    compiled.then_some(directory)
}

/// What the bootstrap names the digest it wrote beside a binary: the binary's
/// own name with this after it.
const DIGEST_SUFFIX: &str = ".sha256";

/// The base the digest's hexadecimal is written in.
const HEXADECIMAL: u32 = 16;

/// The build of this binary, as the bootstrap that installed it wrote it
/// beside it: `<executable>.sha256`, the SHA-256 of the binary's bytes in
/// lowercase hexadecimal on its first line — the file the upload writes and
/// the probe reads.
///
/// Read once, when the daemon starts, and never again: the file is replaced
/// with the binary when another build is installed, while this daemon goes on
/// running the bytes it started from — and what a client must be told is
/// those. A binary run from anywhere else has no such file, and its daemon
/// says nothing about its build.
#[must_use]
pub fn build_beside(executable: &Path) -> Option<BuildDigest> {
    let mut named = executable.as_os_str().to_owned();
    named.push(DIGEST_SUFFIX);
    let written = std::fs::read_to_string(PathBuf::from(named)).ok()?;
    let line = written.lines().next()?.trim();
    let mut digest = [0_u8; DIGEST_LENGTH];
    if line.len() != DIGEST_LENGTH.checked_mul(DIGITS_PER_BYTE)? || !line.is_ascii() {
        return None;
    }
    for (byte, pair) in digest
        .iter_mut()
        .zip(line.as_bytes().chunks(DIGITS_PER_BYTE))
    {
        let pair = core::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, HEXADECIMAL).ok()?;
    }
    Some(BuildDigest(digest))
}

/// How many bytes a SHA-256 is.
const DIGEST_LENGTH: usize = iznik_protocol::identity::BUILD_DIGEST_LENGTH;

/// How many hexadecimal digits a byte is written as.
const DIGITS_PER_BYTE: usize = 2;
