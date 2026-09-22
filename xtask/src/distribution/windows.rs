//! The Windows server, built where the toolchain is native.
//!
//! A Windows host is reached through OpenSSH, which is an optional Windows
//! feature and is off until someone turns it on. The artifact is the same
//! shape as the others: one file named `iznik-server` in the distribution
//! directory, even though cargo writes `iznik-server.exe`. The upload renames
//! it to `iznik-server.exe` on the host, which is the name Windows runs.

use std::path::{Path, PathBuf};

use iznik_harness::process::Output;

use crate::distribution::{DistributionError, linux};

/// The triples this builds for. `x86_64` is what a Windows runner produces.
/// `aarch64` is for a Windows machine of that architecture, built on that
/// machine: a cross link needs an ARM toolchain this does not assume.
pub const TARGETS: &[&str] = &["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];

/// The suffix cargo adds on Windows.
const EXECUTABLE_EXTENSION: &str = "exe";

/// Builds the binary for a Windows `target`, or says this machine cannot.
///
/// # Errors
///
/// [`DistributionError::Build`] when this is not a Windows machine, and
/// whatever cargo says otherwise.
pub fn build(root: &Path, target: &str, output: Output) -> Result<PathBuf, DistributionError> {
    if !cfg!(windows) {
        return Err(DistributionError::Build {
            target: target.to_owned(),
            detail: "a Windows server is built where the toolchain is native, \
                 which is what .github/workflows/snapshot.yml does."
                .to_owned(),
        });
    }
    let placed = linux::build(root, target, output)?;
    let executable = placed.with_extension(EXECUTABLE_EXTENSION);
    if executable.is_file() {
        Ok(executable)
    } else {
        Ok(placed)
    }
}
