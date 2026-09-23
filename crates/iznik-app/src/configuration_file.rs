//! Where this application's own files live, and how one is replaced.
//!
//! The directory is `$XDG_CONFIG_HOME/iznik`, then `~/.config/iznik`, then —
//! on a Windows machine that sets neither variable — `%APPDATA%\iznik`. A file
//! is replaced by writing a temporary file beside it under a name no other
//! writer uses, flushing it to disk, and renaming it over the old one, so a
//! reader sees either the old contents or the new, never part of either.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The directory name under `$HOME` when `XDG_CONFIG_HOME` is unset.
const CONFIGURATION_DIRECTORY: &str = ".config";
/// The directory under the configuration home that holds this application's files.
const APPLICATION_DIRECTORY: &str = "iznik";
/// The environment variable that names the configuration home.
const CONFIGURATION_HOME: &str = "XDG_CONFIG_HOME";
/// The environment variable that names the home directory.
const HOME: &str = "HOME";
/// The environment variable Windows sets to the roaming application data directory.
const APPLICATION_DATA: &str = "APPDATA";
/// The extension of a temporary file before it replaces the real one.
const TEMPORARY_EXTENSION: &str = "temporary";

/// Distinguishes temporary files written by one process at the same time.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// The path of this application's file `name`, or `None` when no
/// configuration home is known, which is when there is nowhere to write.
#[must_use]
pub fn default_path(name: &str) -> Option<PathBuf> {
    let variable = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty());
    let directory = variable(CONFIGURATION_HOME)
        .map(PathBuf::from)
        .or_else(|| variable(HOME).map(|home| PathBuf::from(home).join(CONFIGURATION_DIRECTORY)))
        .or_else(|| variable(APPLICATION_DATA).map(PathBuf::from))?;
    Some(directory.join(APPLICATION_DIRECTORY).join(name))
}

/// Replace the file at `path` with `contents`, creating its directory when it
/// is missing. The previous file is left in place when the new contents
/// cannot be written in full.
///
/// # Errors
///
/// Returns the operating system's error when the directory cannot be
/// created, or the file cannot be written, flushed or renamed into place.
pub fn replace(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(path);
    let written = write_synchronized(&temporary, contents).and_then(|()| rename(&temporary, path));
    if written.is_err() {
        let _removed = std::fs::remove_file(&temporary);
    }
    written
}

/// A temporary name beside `path` that no other write, in this process or
/// another, is using.
fn temporary_path(path: &Path) -> PathBuf {
    let count = WRITES.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{name}.{}.{count}.{TEMPORARY_EXTENSION}",
        std::process::id()
    ))
}

/// Write `contents` to a new file at `path` and flush it to the disk.
///
/// # Errors
///
/// Returns the operating system's error when the file exists already or
/// cannot be written or flushed.
fn write_synchronized(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file: File = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

/// Move `temporary` onto `path`. A rename that cannot replace an existing
/// file, as on some Windows file systems, removes that file and tries once more.
///
/// # Errors
///
/// Returns the operating system's error when the file cannot be replaced.
fn rename(temporary: &Path, path: &Path) -> std::io::Result<()> {
    if std::fs::rename(temporary, path).is_ok() {
        return Ok(());
    }
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temporary, path)
}
