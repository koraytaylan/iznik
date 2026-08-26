//! The gates, the policy checks, the claims registry, the images, the staging, the distribution, the header and the soak behind `cargo xtask`.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

pub mod claims;
pub mod distribution;
pub mod doctor;
pub mod gate;
pub mod header;
pub mod policy;
pub mod regression;
pub mod soak;

/// The exit code of a command line the binary cannot act on: a subcommand it
/// does not know, a flag it cannot parse, or a subcommand whose task has not
/// landed yet. Two, the conventional usage-error status, so that a failed run's
/// one and a refused command line are told apart.
pub const USAGE_EXIT_CODE: u8 = 2;

/// The repository root.
///
/// `xtask` is only ever run through `cargo xtask` from inside this repository,
/// so the root is the parent of this crate's directory, known when the crate
/// is compiled; it is not derived from the current directory, which is
/// wherever the person invoking cargo happens to be.
#[must_use]
pub fn repository_root() -> PathBuf {
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_directory
        .parent()
        .map_or_else(|| manifest_directory.to_path_buf(), Path::to_path_buf)
}
