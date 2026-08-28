//! The two Linux musl targets: stripped, statically linked, reproducible.
//!
//! musl because the bootstrap does not know the host's libc version and must
//! not care; static because a shared object it also had to upload would be a
//! second thing to get right; reproducible because a digest is only worth
//! having if two builds of one commit agree on it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};

use crate::distribution::{BINARY, DistributionError};

/// The triples this builds for.
pub const TARGETS: &[&str] = &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

/// The profile the artifacts are built under.
const PROFILE: &str = "release";

/// How long a release build may take. Two targets from cold is minutes, and a
/// build that hangs must be a named failure rather than a wait.
const BUILD_DEADLINE: Duration = Duration::from_mins(30);

/// The workspace's cargo configuration, relative to the root.
const CARGO_CONFIG: &str = ".cargo/config.toml";

/// What makes two builds of one commit agree: the absolute path of the
/// checkout, which otherwise reaches the binary through panic locations, is
/// replaced by a name that is the same everywhere.
const REMAPPED: &str = ".";

/// Builds the binary for `target` and says where cargo put it.
///
/// # Errors
///
/// [`DistributionError::Build`] when the build fails or its toolchain is
/// absent, naming what cargo said.
pub fn build(root: &Path, target: &str) -> Result<PathBuf, DistributionError> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(root)
        .arg("build")
        .arg("--locked")
        .arg("--profile")
        .arg(PROFILE)
        .arg("--target")
        .arg(target)
        .arg("--package")
        .arg(BINARY)
        .arg("--bin")
        .arg(BINARY)
        .env("RUSTFLAGS", rustflags(root, target));
    // Its output is cargo's own progress; what matters is that it succeeded.
    process::run(command, Deadline(BUILD_DEADLINE), Output::Capture).map_err(|source| {
        DistributionError::Build {
            target: target.to_owned(),
            detail: source.to_string(),
        }
    })?;
    Ok(built_at(root, target))
}

/// The flags this build needs: what the workspace configures for the target,
/// and the remapping that makes two builds of one commit agree.
///
/// `RUSTFLAGS` replaces every configured flag rather than adding to them, so
/// the configured ones are read and passed on. Without that, the musl targets
/// lose `link-self-contained=no` and link rustc's startup objects beside the
/// cross-compiler's, which is a duplicate `_start` and nothing else.
#[must_use]
pub fn rustflags(root: &Path, target: &str) -> String {
    let mut flags = configured(root, target);
    flags.push(format!("--remap-path-prefix={}={REMAPPED}", root.display()));
    flags.join(" ")
}

/// The flags `.cargo/config.toml` sets for a target, and for every build.
fn configured(root: &Path, target: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(CARGO_CONFIG)) else {
        return Vec::new();
    };
    let Ok(config) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let build = config.get("build").and_then(|table| table.get("rustflags"));
    let targeted = config
        .get("target")
        .and_then(|table| table.get(target))
        .and_then(|table| table.get("rustflags"));
    [build, targeted]
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_array)
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Where cargo puts the binary for a target under the release profile.
#[must_use]
pub fn built_at(root: &Path, target: &str) -> PathBuf {
    let base =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    base.join(target).join(PROFILE).join(BINARY)
}
