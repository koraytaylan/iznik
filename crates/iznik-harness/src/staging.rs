//! Staging the three static musl binaries the containers run, keyed by
//! content hash, a no-op when nothing changed: `iznik-server`,
//! `iznik-regression` and `iznik`, built under the `regression` profile for
//! this machine's architecture, laid out as `bin/` and
//! `distribution/<target>/iznik-server` under a directory named by the hash
//! of the three. `IZNIK_STAGED`, when set, names a directory used instead
//! of building, so whatever stages once can hand the result to many.

use std::fmt::{self, Display, Formatter, Write as _};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, id};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::images::repository_root;
use crate::process::{self, Deadline, Output, ProcessError};

/// How long staging may take: a cold musl build compiles the emulator.
pub const STAGING_DEADLINE: Duration = Duration::from_mins(15);

/// The variable that names a staged directory to use instead of building.
pub const STAGED_VARIABLE: &str = "IZNIK_STAGED";

/// The target the containers run: this machine's own architecture, as a musl
/// triple.
///
/// The fixture pulls the container image for the machine it runs on, so the
/// binaries that run inside it must be built for that same machine — an
/// architecture chosen by hand would put the emulated case back for whoever
/// has the other one.
#[must_use]
pub fn target() -> String {
    format!("{}-unknown-linux-musl", std::env::consts::ARCH)
}

/// The profile the binaries are built under.
pub const PROFILE: &str = "regression";

/// The variable cargo reads for its target directory.
const TARGET_DIRECTORY_VARIABLE: &str = "CARGO_TARGET_DIR";

/// Cargo's default target directory, under the repository root.
const DEFAULT_TARGET_DIRECTORY: &str = "target";

/// Where staged directories live, under the target directory.
const STAGING_DIRECTORY: &str = "regression-staging";

/// The staged layout's binaries directory.
const BIN_DIRECTORY: &str = "bin";

/// The staged layout's distribution directory, where the bootstrap looks.
const DISTRIBUTION_DIRECTORY: &str = "distribution";

/// The binary the distribution directory carries.
const DISTRIBUTED_BINARY: &str = "iznik-server";

/// The packages built, and the binaries they produce, in hash order.
const BUILT: &[(&str, &str)] = &[
    ("iznik-server", "iznik-server"),
    ("iznik-regression", "iznik-regression"),
    ("iznik-cli", "iznik"),
];

/// How staging finds cargo, the target directory and an override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagingOptions {
    /// A directory to use instead of building, when one is named.
    pub staged: Option<PathBuf>,
    /// The cargo program.
    pub cargo: PathBuf,
    /// The target directory cargo builds into.
    pub target_directory: PathBuf,
}

impl StagingOptions {
    /// The options the environment gives: [`STAGED_VARIABLE`] as the
    /// override, `cargo` on the path, and cargo's target directory.
    #[must_use]
    pub fn from_environment() -> StagingOptions {
        StagingOptions {
            staged: std::env::var_os(STAGED_VARIABLE).map(PathBuf::from),
            cargo: PathBuf::from("cargo"),
            target_directory: std::env::var_os(TARGET_DIRECTORY_VARIABLE).map_or_else(
                || repository_root().join(DEFAULT_TARGET_DIRECTORY),
                PathBuf::from,
            ),
        }
    }
}

/// Why staging could not finish.
#[derive(Debug)]
pub enum StagingError {
    /// The build failed or exceeded its deadline.
    Build {
        /// What went wrong, boxed to keep the error small.
        source: Box<ProcessError>,
    },
    /// A binary the build should have produced is not there.
    Missing {
        /// Where it was expected.
        path: PathBuf,
    },
    /// A file could not be read, copied or a directory created.
    Io {
        /// The path concerned.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
}

impl Display for StagingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            StagingError::Build { source } => {
                write!(formatter, "the staging build failed: {source}")
            }
            StagingError::Missing { path } => {
                write!(formatter, "the build produced no {}", path.display())
            }
            StagingError::Io { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for StagingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StagingError::Build { source } => Some(source.as_ref()),
            StagingError::Io { source, .. } => Some(source),
            StagingError::Missing { .. } => None,
        }
    }
}

/// The error for an operating-system failure on a path.
fn io_error(path: &Path, source: io::Error) -> StagingError {
    StagingError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Runs the build of the three packages.
///
/// # Errors
///
/// [`StagingError::Build`] when cargo fails or exceeds the deadline.
fn build(options: &StagingOptions, deadline: Deadline) -> Result<(), StagingError> {
    let target = target();
    let mut command = Command::new(&options.cargo);
    command
        .args(["build", "--profile", PROFILE, "--target", &target])
        .current_dir(repository_root())
        .env(TARGET_DIRECTORY_VARIABLE, &options.target_directory);
    for (package, _binary) in BUILT {
        command.args(["--package", package]);
    }
    process::run(command, deadline, Output::Inherit)
        .map(|_completed| ())
        .map_err(|source| StagingError::Build {
            source: Box::new(source),
        })
}

/// The built binaries in hash order, each read whole.
///
/// # Errors
///
/// [`StagingError::Missing`] when one is not there, [`StagingError::Io`]
/// when one cannot be read.
fn built_binaries(options: &StagingOptions) -> Result<Vec<(PathBuf, Vec<u8>)>, StagingError> {
    let built = options.target_directory.join(target()).join(PROFILE);
    BUILT
        .iter()
        .map(|(_package, binary)| {
            let path = built.join(binary);
            if !path.is_file() {
                return Err(StagingError::Missing { path });
            }
            let bytes = std::fs::read(&path).map_err(|source| io_error(&path, source))?;
            Ok((path, bytes))
        })
        .collect()
}

/// The hex SHA-256 over the binaries' bytes, in order.
fn content_hash(binaries: &[(PathBuf, Vec<u8>)]) -> String {
    let mut hasher = Sha256::new();
    for (_path, bytes) in binaries {
        hasher.update(bytes);
    }
    hasher
        .finalize()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            // Writing to a String cannot fail.
            let _written = write!(hex, "{byte:02x}");
            hex
        })
}

/// The paths a staged directory must hold to be complete.
fn expected_layout(staged: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = BUILT
        .iter()
        .map(|(_package, binary)| staged.join(BIN_DIRECTORY).join(binary))
        .collect();
    paths.push(
        staged
            .join(DISTRIBUTION_DIRECTORY)
            .join(target())
            .join(DISTRIBUTED_BINARY),
    );
    paths
}

/// Copies one built binary to a staged path, creating its directory.
///
/// # Errors
///
/// [`StagingError::Io`] when the directory cannot be made or the copy fails.
fn place(from: &Path, to: &Path) -> Result<(), StagingError> {
    if let Some(directory) = to.parent() {
        std::fs::create_dir_all(directory).map_err(|source| io_error(directory, source))?;
    }
    std::fs::copy(from, to)
        .map(|_bytes| ())
        .map_err(|source| io_error(to, source))
}

/// The staged directory, built and laid out when it does not exist yet.
///
/// # Errors
///
/// [`StagingError`] when the build, a read or a copy fails.
pub fn stage_with(options: &StagingOptions, deadline: Deadline) -> Result<PathBuf, StagingError> {
    if let Some(staged) = &options.staged {
        return Ok(staged.clone());
    }
    build(options, deadline)?;
    let binaries = built_binaries(options)?;
    let root = options.target_directory.join(STAGING_DIRECTORY);
    let staged = root.join(content_hash(&binaries));
    if expected_layout(&staged).iter().all(|path| path.is_file()) {
        return Ok(staged);
    }
    // Lay the tree out under a partial directory named for this process, then
    // move it into place with one rename, so a kill mid-copy never leaves a
    // truncated binary that the completeness check above would trust.
    let partial = root.join(format!("partial-{}", id()));
    let _cleared = std::fs::remove_dir_all(&partial);
    for ((path, _bytes), (_package, binary)) in binaries.iter().zip(BUILT) {
        place(path, &partial.join(BIN_DIRECTORY).join(binary))?;
        if *binary == DISTRIBUTED_BINARY {
            place(
                path,
                &partial
                    .join(DISTRIBUTION_DIRECTORY)
                    .join(target())
                    .join(DISTRIBUTED_BINARY),
            )?;
        }
    }
    if std::fs::rename(&partial, &staged).is_err() {
        // Another stager placed it first, or the hash directory exists; the
        // partial is ours to drop, and the placed tree is what we return.
        let _dropped = std::fs::remove_dir_all(&partial);
        if !expected_layout(&staged).iter().all(|path| path.is_file()) {
            return Err(StagingError::Missing {
                path: staged.join(BIN_DIRECTORY).join(DISTRIBUTED_BINARY),
            });
        }
    }
    Ok(staged)
}

/// The staged directory as the environment configures it: what
/// [`STAGED_VARIABLE`] names, or a build keyed by content hash.
///
/// # Errors
///
/// [`StagingError`] when the build, a read or a copy fails.
pub fn stage(deadline: Deadline) -> Result<PathBuf, StagingError> {
    stage_with(&StagingOptions::from_environment(), deadline)
}
