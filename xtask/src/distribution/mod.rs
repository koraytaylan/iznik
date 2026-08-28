//! `xtask distribution --target <triple>`: reproducible release artifacts with
//! checksums and a manifest.
//!
//! The bootstrap uploads one file to a host whose libc version it does not
//! know, so the artifact is statically linked and stripped, and reproducible
//! from the same commit so that two people who build it get the same bytes.
//! The manifest is for a person and for a pipeline; the bootstrap computes
//! digests itself and trusts nothing written beside the file.

pub mod darwin;
pub mod linux;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use core::fmt::{self, Display, Formatter, Write as _};

use sha2::{Digest, Sha256};

/// The most one artifact may weigh. A binary a bootstrap uploads over a slow
/// link is a binary somebody waits for.
pub const ARTIFACT_SIZE_CEILING: u64 = 24 * 1024 * 1024;

/// The binary that is distributed.
pub const BINARY: &str = "iznik-server";

/// Where artifacts are put, under cargo's target directory.
pub const DISTRIBUTION_DIRECTORY: &str = "distribution";

/// The checksum file beside every artifact.
pub const CHECKSUMS: &str = "SHA256SUMS";

/// The manifest beside it.
pub const MANIFEST: &str = "manifest.toml";

/// The flag the subcommand takes.
const TARGET_FLAG: &str = "--target";

/// Why an artifact could not be made.
#[derive(Debug)]
pub enum DistributionError {
    /// The triple is not one this knows how to build.
    UnknownTarget {
        /// What was asked for.
        target: String,
    },
    /// The build failed, or the toolchain for it is not there.
    Build {
        /// What was asked for.
        target: String,
        /// What the build said.
        detail: String,
    },
    /// A file could not be read, written or copied.
    Io {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The artifact is bigger than the ceiling.
    Oversize {
        /// How big it is.
        bytes: u64,
        /// What it may be.
        ceiling: u64,
    },
}

impl Display for DistributionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            DistributionError::UnknownTarget { target } => {
                write!(formatter, "{target}: not a target this builds for")
            }
            DistributionError::Build { target, detail } => {
                write!(formatter, "{target}: {detail}")
            }
            DistributionError::Io { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
            DistributionError::Oversize { bytes, ceiling } => write!(
                formatter,
                "the artifact is {bytes} bytes, over the {ceiling} ceiling"
            ),
        }
    }
}

impl core::error::Error for DistributionError {}

/// One built artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// The triple it was built for.
    pub target: String,
    /// Where it is.
    pub path: PathBuf,
    /// Its `SHA256` digest, in hexadecimal.
    pub digest: String,
    /// How many bytes it is.
    pub bytes: u64,
}

/// The `SHA256` of a file, in hexadecimal.
///
/// # Errors
///
/// [`DistributionError::Io`] when it cannot be read.
pub fn digest_of(path: &Path) -> Result<String, DistributionError> {
    let bytes = std::fs::read(path).map_err(|source| DistributionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut held, byte| {
            // A digit that will not format is not a thing that happens; there
            // is nowhere to report it to from inside a fold.
            let _written = write!(held, "{byte:02x}");
            held
        }))
}

/// Builds the artifact for a triple, writes its checksum file and its
/// manifest beside it, and says what it made.
///
/// # Errors
///
/// [`DistributionError::UnknownTarget`] for a triple this does not build,
/// [`DistributionError::Build`] when the build fails or its toolchain is
/// absent, [`DistributionError::Io`] when a file cannot be written, and
/// [`DistributionError::Oversize`] when the result is over the ceiling.
pub fn build(root: &Path, target: &str) -> Result<Artifact, DistributionError> {
    let built = if linux::TARGETS.contains(&target) {
        linux::build(root, target)?
    } else if darwin::TARGETS.contains(&target) {
        darwin::build(root, target)?
    } else {
        return Err(DistributionError::UnknownTarget {
            target: target.to_owned(),
        });
    };
    let directory = target_directory(root)
        .join(DISTRIBUTION_DIRECTORY)
        .join(target);
    std::fs::create_dir_all(&directory).map_err(|source| DistributionError::Io {
        path: directory.clone(),
        source,
    })?;
    let path = directory.join(BINARY);
    let _copied = std::fs::copy(&built, &path).map_err(|source| DistributionError::Io {
        path: built.clone(),
        source,
    })?;
    let bytes = std::fs::metadata(&path)
        .map_err(|source| DistributionError::Io {
            path: path.clone(),
            source,
        })?
        .len();
    if bytes > ARTIFACT_SIZE_CEILING {
        return Err(DistributionError::Oversize {
            bytes,
            ceiling: ARTIFACT_SIZE_CEILING,
        });
    }
    let artifact = Artifact {
        target: target.to_owned(),
        digest: digest_of(&path)?,
        bytes,
        path,
    };
    record(root, &directory, &artifact)?;
    Ok(artifact)
}

/// Writes the checksum file and the manifest beside an artifact.
///
/// # Errors
///
/// [`DistributionError::Io`] when either cannot be written.
fn record(root: &Path, directory: &Path, artifact: &Artifact) -> Result<(), DistributionError> {
    let sums = directory.join(CHECKSUMS);
    // The shape `sha256sum -c` reads: the digest, two spaces, the name.
    let line = format!("{}  {BINARY}\n", artifact.digest);
    std::fs::write(&sums, line).map_err(|source| DistributionError::Io { path: sums, source })?;
    let manifest = directory.join(MANIFEST);
    let said = format!(
        "crate_version = \"{}\"\nprotocol_version = {}\ntarget = \"{}\"\nbinary = \"{BINARY}\"\nbytes = {}\nsha256 = \"{}\"\n",
        crate_version(),
        protocol_version(root).unwrap_or_default(),
        artifact.target,
        artifact.bytes,
        artifact.digest
    );
    std::fs::write(&manifest, said).map_err(|source| DistributionError::Io {
        path: manifest,
        source,
    })
}

/// The version the distributed binary carries. Every crate in this workspace
/// takes its version from the workspace, so this crate's is that one.
fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Where the protocol version is declared.
const PROTOCOL_SOURCE: &str = "crates/iznik-protocol/src/message.rs";

/// The line it is declared on.
const PROTOCOL_DECLARATION: &str = "pub const PROTOCOL_VERSION: u16 = ";

/// The protocol version the distributed binary speaks, read from the source
/// rather than from the crate: a tooling crate reaches neither the emulator
/// nor the product, and the workspace has a test that says so.
fn protocol_version(root: &Path) -> Option<u16> {
    let source = std::fs::read_to_string(root.join(PROTOCOL_SOURCE)).ok()?;
    source
        .lines()
        .find_map(|line| line.trim().strip_prefix(PROTOCOL_DECLARATION))
        .and_then(|rest| rest.trim_end_matches(';').trim().parse().ok())
}

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and this parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let mut rest = arguments.iter().skip(1);
    let mut wanted: Option<String> = None;
    while let Some(argument) = rest.next() {
        if argument.to_string_lossy() != TARGET_FLAG {
            return refuse(&format!("{}: unknown flag", argument.to_string_lossy()));
        }
        let Some(target) = rest.next() else {
            return refuse(&format!("{TARGET_FLAG}: a triple must follow"));
        };
        wanted = Some(target.to_string_lossy().into_owned());
    }
    let Some(target) = wanted else {
        return refuse(&format!("usage: xtask distribution {TARGET_FLAG} <triple>"));
    };
    let root = workspace_root();
    match build(&root, &target) {
        Ok(artifact) => {
            say(&format!(
                "{} {} bytes {}",
                artifact.path.display(),
                artifact.bytes,
                artifact.digest
            ));
            ExitCode::SUCCESS
        }
        Err(error) => refuse(&error.to_string()),
    }
}

/// Cargo's target directory: what `CARGO_TARGET_DIR` names, or `target` under
/// the root. Artifacts go beside what built them, wherever that is.
#[must_use]
pub fn target_directory(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// The workspace root: one directory above this crate's manifest.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Says something on standard output.
fn say(line: &str) {
    let mut stdout = std::io::stdout();
    let _written = std::io::Write::write_all(&mut stdout, format!("{line}\n").as_bytes());
    let _flushed = std::io::Write::flush(&mut stdout);
}

/// Says something on standard error, and refuses.
fn refuse(line: &str) -> ExitCode {
    let mut stderr = std::io::stderr();
    let _written = std::io::Write::write_all(&mut stderr, format!("{line}\n").as_bytes());
    let _flushed = std::io::Write::flush(&mut stderr);
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
