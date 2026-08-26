//! The container images pinned by digest and tagged by content hash, built
//! only when missing. A tag is the SHA-256 of the Containerfile it was
//! built from, so it is an honest cache key: the image for a file's bytes
//! exists or it does not, and nothing stale can answer to the name.

use std::fmt::{self, Display, Formatter, Write as _};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::process::{self, Deadline, Output, ProcessError};

/// How long a build may take: the first build pulls a base image.
pub const IMAGE_BUILD_DEADLINE: Duration = Duration::from_mins(15);

/// The container engine.
pub const PROGRAM: &str = "podman";

/// Where the Containerfiles live, relative to the repository root.
const IMAGES_DIRECTORY: &str = "regression/images";

/// The host image's Containerfile, in that directory.
const HOST_CONTAINERFILE: &str = "Containerfile.host";

/// The engine image's Containerfile, in that directory.
const ENGINE_CONTAINERFILE: &str = "Containerfile.engine";

/// The host image's name, tagged by content.
const HOST_IMAGE: &str = "localhost/iznik-host";

/// The engine image's name, tagged by content.
const ENGINE_IMAGE: &str = "localhost/iznik-engine";

/// The exit status `podman image exists` gives for an image that does not.
const ABSENT_STATUS: i32 = 1;

/// How many directories above this crate's manifest the repository root
/// is: the crate lives at `crates/<name>`.
const ROOT_DEPTH: usize = 2;

/// The two images, as references the engine resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Images {
    /// The host image, as `localhost/iznik-host:<tag>`.
    pub host: String,
    /// The engine image, as `localhost/iznik-engine:<tag>`.
    pub engine: String,
}

/// Why an image could not be ensured.
#[derive(Debug)]
pub enum ImagesError {
    /// A Containerfile could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The container engine could not do what was asked, or is not there.
    Engine {
        /// The engine program, as asked for.
        program: String,
        /// What was asked: `build`, or `image exists`.
        action: &'static str,
        /// The image concerned.
        image: String,
        /// What went wrong, boxed to keep the error small.
        source: Box<ProcessError>,
    },
}

impl Display for ImagesError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ImagesError::Read { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            ImagesError::Engine {
                program,
                action,
                image,
                source,
            } => write!(
                formatter,
                "`{program} {action}` failed for {image}: {source}"
            ),
        }
    }
}

impl std::error::Error for ImagesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ImagesError::Read { source, .. } => Some(source),
            ImagesError::Engine { source, .. } => Some(source.as_ref()),
        }
    }
}

/// The repository root, two directories above this crate's manifest: where
/// this crate's fixture, staging and images find the files they need.
#[must_use]
pub fn repository_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(ROOT_DEPTH)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

/// The directory the Containerfiles live in.
fn images_directory() -> PathBuf {
    repository_root().join(IMAGES_DIRECTORY)
}

/// The tag for a Containerfile: the hex SHA-256 of its bytes, so the same
/// content always names the same image and a changed byte names another.
///
/// # Errors
///
/// [`ImagesError::Read`] when the file cannot be read.
pub fn image_tag(containerfile: &Path) -> Result<String, ImagesError> {
    let bytes = std::fs::read(containerfile).map_err(|source| ImagesError::Read {
        path: containerfile.to_path_buf(),
        source,
    })?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            // Writing to a String cannot fail.
            let _written = write!(hex, "{byte:02x}");
            hex
        }))
}

/// The engine error for a failed command.
fn engine_error(
    program: &Path,
    action: &'static str,
    image: &str,
    source: ProcessError,
) -> ImagesError {
    ImagesError::Engine {
        program: program.display().to_string(),
        action,
        image: image.to_owned(),
        source: Box::new(source),
    }
}

/// Whether the engine already holds an image by this reference.
///
/// # Errors
///
/// [`ImagesError::Engine`] when the engine cannot be asked.
fn exists(program: &Path, reference: &str, deadline: Deadline) -> Result<bool, ImagesError> {
    let mut command = Command::new(program);
    command.args(["image", "exists", reference]);
    match process::run(command, deadline, Output::Capture) {
        Ok(_completed) => Ok(true),
        Err(ProcessError::Failed { status, .. }) if status.code() == Some(ABSENT_STATUS) => {
            Ok(false)
        }
        Err(source) => Err(engine_error(program, "image exists", reference, source)),
    }
}

/// Builds an image from a Containerfile under the reference.
///
/// # Errors
///
/// [`ImagesError::Engine`] when the build fails or exceeds the deadline.
fn build(
    program: &Path,
    containerfile: &Path,
    reference: &str,
    deadline: Deadline,
) -> Result<(), ImagesError> {
    let mut command = Command::new(program);
    command
        .arg("build")
        .arg("--file")
        .arg(containerfile)
        .arg("--tag")
        .arg(reference)
        .arg(images_directory());
    process::run(command, deadline, Output::Inherit)
        .map(|_completed| ())
        .map_err(|source| engine_error(program, "build", reference, source))
}

/// One image: its reference, built when the engine does not hold it.
///
/// # Errors
///
/// [`ImagesError`] when the Containerfile cannot be read or the engine
/// cannot answer or build.
fn ensure(
    program: &Path,
    containerfile: &str,
    image: &str,
    deadline: Deadline,
) -> Result<String, ImagesError> {
    let path = images_directory().join(containerfile);
    let reference = format!("{image}:{}", image_tag(&path)?);
    if !exists(program, &reference, deadline)? {
        build(program, &path, &reference, deadline)?;
    }
    Ok(reference)
}

/// Both images, building only what is missing, each build under the
/// deadline; the engine is [`PROGRAM`] on the path. Nothing serializes
/// concurrent callers: two that find an image missing both build it, and
/// the last to finish owns the tag, so whatever runs tests in parallel
/// ensures the images once first.
///
/// # Errors
///
/// [`ImagesError`], naming the engine program when it is not there.
pub fn ensure_images(deadline: Deadline) -> Result<Images, ImagesError> {
    ensure_images_with(Path::new(PROGRAM), deadline)
}

/// Both images through a given engine program: what [`ensure_images`] does,
/// with the program a parameter so a missing one can be proven to be
/// reported.
///
/// # Errors
///
/// [`ImagesError`], naming the program when it is not there.
pub fn ensure_images_with(program: &Path, deadline: Deadline) -> Result<Images, ImagesError> {
    let host = ensure(program, HOST_CONTAINERFILE, HOST_IMAGE, deadline)?;
    let engine = ensure(program, ENGINE_CONTAINERFILE, ENGINE_IMAGE, deadline)?;
    Ok(Images { host, engine })
}
