//! The two Darwin targets, which need a toolchain a Linux machine does not
//! have.
//!
//! Remote hosts are not always Linux — a Mac under a desk is a common target —
//! so the manifest and checksum path is the same one. What differs is that
//! cross-compiling here needs an SDK, and when it is not there the failure
//! says which component is missing rather than reporting a link error from
//! deep inside cargo. The workflow builds these where the toolchain is native.

use std::path::{Path, PathBuf};

use crate::distribution::{DistributionError, linux};

/// The triples this builds for.
pub const TARGETS: &[&str] = &["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// What a Darwin cross build needs and a Linux machine does not have.
const SDK_VARIABLE: &str = "SDKROOT";

/// Builds the binary for a Darwin `target`, or says what is missing.
///
/// # Errors
///
/// [`DistributionError::Build`] naming the missing toolchain component when
/// the SDK is not configured, and whatever cargo says otherwise.
pub fn build(root: &Path, target: &str) -> Result<PathBuf, DistributionError> {
    if std::env::var_os(SDK_VARIABLE).is_none() {
        return Err(DistributionError::Build {
            target: target.to_owned(),
            detail: format!(
                "no Darwin SDK: {SDK_VARIABLE} is unset, and cross-compiling to Darwin needs one. \
                 Build these where the toolchain is native, which is what \
                 .github/workflows/darwin-artifacts.yml does."
            ),
        });
    }
    // The same build as the Linux targets: only the toolchain differs, and
    // the manifest and checksum path is shared.
    linux::build(root, target)
}
