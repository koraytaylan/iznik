//! Deterministic application bundle layout writers.

use std::fs;
use std::io;
use std::path::Path;

/// Stable bundle identifier used by both platform layouts.
const APPLICATION_IDENTIFIER: &str = "org.iznik.client";
/// Product executable and display name.
const APPLICATION_NAME: &str = "iznik";

/// Write a Linux binary layout and desktop entry from a staged executable.
///
/// # Errors
///
/// Returns an I/O error when the input is missing or output cannot be written.
pub fn write_linux(binary: &Path, output: &Path, version: &str) -> io::Result<()> {
    let bytes = fs::read(binary)?;
    fs::create_dir_all(output.join("bin"))?;
    fs::write(output.join("bin").join(APPLICATION_NAME), bytes)?;
    let desktop = format!(
        "[Desktop Entry]\nName={APPLICATION_NAME}\nExec={APPLICATION_NAME}\nVersion={version}\nType=Application\n"
    );
    fs::write(output.join("iznik.desktop"), desktop)
}

/// Write a macOS `.app` bundle with a version-stamped property list.
///
/// # Errors
///
/// Returns an I/O error when the input is missing or output cannot be written.
pub fn write_macos(binary: &Path, output: &Path, version: &str) -> io::Result<()> {
    let bytes = fs::read(binary)?;
    let contents = output.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::write(contents.join("MacOS").join(APPLICATION_NAME), bytes)?;
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist><dict><key>CFBundleIdentifier</key><string>{APPLICATION_IDENTIFIER}</string><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleName</key><string>{APPLICATION_NAME}</string></dict></plist>\n"
    );
    fs::write(contents.join("Info.plist"), plist)
}
