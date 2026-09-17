//! The two Darwin targets, which need a toolchain a Linux machine does not
//! have.
//!
//! Remote hosts are not always Linux — a Mac under a desk is a common target —
//! so the manifest and checksum path is the same one. What differs is the
//! toolchain, and what it needs is this:
//!
//! - **The SDK.** Apple's, from Xcode or the Command Line Tools. It is not
//!   redistributable, so a Linux machine has one only if somebody put it
//!   there. On a Mac, `xcode-select --install` is enough.
//! - **`SDKROOT`.** The absolute path of that SDK, exported. On a Mac,
//!   `xcrun --show-sdk-path` prints it; the workflow sets it from there. This
//!   is the one variable this module checks, because it is the one a build
//!   cannot proceed without and the one a cross-compiling setup forgets.
//! - **The linker.** Apple's `ld`, reached through `cc` — native on a Mac,
//!   and on anything else a cross linker configured under
//!   `[target.<triple>]` in `.cargo/config.toml`, the same place the musl
//!   targets configure theirs.
//! - **The targets.** `rustup target add aarch64-apple-darwin
//!   x86_64-apple-darwin`; `rust-toolchain.toml` pins only the musl pair,
//!   because those are what the containers and the bootstrap run.
//!
//! When the SDK is not there the failure says so and names the workflow that
//! builds these where the toolchain is native, rather than reporting a link
//! error from deep inside cargo in the middle of somebody's release.

use std::path::{Path, PathBuf};

use iznik_harness::process::Output;

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
pub fn build(root: &Path, target: &str, output: Output) -> Result<PathBuf, DistributionError> {
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
    linux::build(root, target, output)
}
