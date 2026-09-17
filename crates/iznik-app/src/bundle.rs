//! Deterministic application bundle layout writers, and where a bundled
//! application finds the servers it carries.
//!
//! A host reached over ssh gets the server this build carries uploaded to it,
//! so a bundle carries every server a host might need: one
//! `<triple>/iznik-server` per triple, laid out as `xtask distribution` lays
//! them out and as `ArtifactSet::load` reads them. On Linux they sit under
//! `share/iznik/artifacts` beside `bin/`; in a macOS bundle under
//! `Contents/Resources/artifacts` beside `Contents/MacOS/`. Both are two
//! directories up from the executable, which is how the running application
//! finds them. So is `distribution` in cargo's target directory, where
//! `cargo xtask distribution` writes them, for an application run with
//! `cargo run`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Stable bundle identifier used by both platform layouts.
const APPLICATION_IDENTIFIER: &str = "org.iznik.client";
/// Product executable and display name.
const APPLICATION_NAME: &str = "iznik";
/// The server binary each triple directory holds.
const SERVER_BINARY: &str = "iznik-server";
/// Where a Linux layout keeps its servers, under the layout's root.
const LINUX_SERVERS: &[&str] = &["share", "iznik", "artifacts"];
/// Where a macOS bundle keeps its servers, under `Contents`.
const MACOS_SERVERS: &[&str] = &["Resources", "artifacts"];
/// Where `cargo xtask distribution` writes servers, under cargo's target
/// directory, which is two up from a binary `cargo run` builds.
const DISTRIBUTION_SERVERS: &[&str] = &["distribution"];

/// Write a Linux binary layout, its servers and a desktop entry from a staged
/// executable and a directory of servers.
///
/// # Errors
///
/// Returns an I/O error when an input is missing, when `servers` holds no
/// `<triple>/iznik-server`, or when output cannot be written.
pub fn write_linux(binary: &Path, servers: &Path, output: &Path, version: &str) -> io::Result<()> {
    fs::create_dir_all(output.join("bin"))?;
    // Copied rather than read and written, so the executable stays executable.
    let _bytes = fs::copy(binary, output.join("bin").join(APPLICATION_NAME))?;
    copy_servers(servers, &joined(output, LINUX_SERVERS))?;
    let desktop = format!(
        "[Desktop Entry]\nName={APPLICATION_NAME}\nExec={APPLICATION_NAME}\nVersion={version}\nType=Application\n"
    );
    fs::write(output.join("iznik.desktop"), desktop)
}

/// Write a macOS `.app` bundle with its servers and a version-stamped
/// property list.
///
/// # Errors
///
/// Returns an I/O error when an input is missing, when `servers` holds no
/// `<triple>/iznik-server`, or when output cannot be written.
pub fn write_macos(binary: &Path, servers: &Path, output: &Path, version: &str) -> io::Result<()> {
    let contents = output.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    let _bytes = fs::copy(binary, contents.join("MacOS").join(APPLICATION_NAME))?;
    copy_servers(servers, &joined(&contents, MACOS_SERVERS))?;
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist><dict><key>CFBundleIdentifier</key><string>{APPLICATION_IDENTIFIER}</string><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleName</key><string>{APPLICATION_NAME}</string></dict></plist>\n"
    );
    fs::write(contents.join("Info.plist"), plist)
}

/// The servers an application carries, found from its own executable: the
/// Linux layout's `share/iznik/artifacts`, the macOS bundle's
/// `Contents/Resources/artifacts`, or, for a workspace build, the target
/// directory's `distribution` — the first that holds a server.
#[must_use]
pub fn bundled_servers(executable: &Path) -> Option<PathBuf> {
    let root = executable.parent()?.parent()?;
    [
        joined(root, LINUX_SERVERS),
        joined(root, MACOS_SERVERS),
        joined(root, DISTRIBUTION_SERVERS),
    ]
    .into_iter()
    .find(|candidate| holds_a_server(candidate))
}

/// Whether a directory holds at least one `<triple>/iznik-server`.
fn holds_a_server(directory: &Path) -> bool {
    fs::read_dir(directory).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.path().join(SERVER_BINARY).is_file())
    })
}

/// A path with every segment appended.
fn joined(base: &Path, segments: &[&str]) -> PathBuf {
    segments
        .iter()
        .fold(base.to_path_buf(), |path, segment| path.join(segment))
}

/// Copy every `<triple>/iznik-server` under `servers` to the same place under
/// `destination`, leaving checksums and manifests behind; the bootstrap
/// computes digests from the bytes it sends.
///
/// # Errors
///
/// Returns an I/O error when `servers` cannot be read or a copy fails, and a
/// `NotFound` error when it holds no server at all: a bundle that could
/// install nothing on an ssh host is not one to ship.
fn copy_servers(servers: &Path, destination: &Path) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(servers)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut copied = false;
    for entry in entries {
        let source = entry.path().join(SERVER_BINARY);
        if !source.is_file() {
            continue;
        }
        let target = destination.join(entry.file_name());
        fs::create_dir_all(&target)?;
        // `fs::copy` carries the permission bits, so the server stays executable.
        let _bytes = fs::copy(&source, target.join(SERVER_BINARY))?;
        copied = true;
    }
    if copied {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no <triple>/{SERVER_BINARY} under {}; build them with `cargo xtask distribution --target <triple>`",
                servers.display()
            ),
        ))
    }
}
