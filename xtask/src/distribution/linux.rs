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

/// And the same for the dependencies, which carry their own panic locations
/// out of a registry under a home directory that differs on every machine.
/// Stripping does not remove these: they are data, not symbols.
const REMAPPED_REGISTRY: &str = "registry";

/// Where cargo unpacks what it downloads, under `CARGO_HOME`.
const REGISTRY_SOURCES: &str = "registry/src";

/// Where cargo keeps a dependency it took from a git remote — the emulator is
/// one — under `CARGO_HOME`.
const GIT_CHECKOUTS: &str = "git/checkouts";

/// And the name that stands in for it.
const REMAPPED_CHECKOUTS: &str = "checkouts";

/// The variable this sets. Cargo prefers it to `RUSTFLAGS` outright, and its
/// arguments are separated by a unit separator rather than by whitespace — so
/// a flag that contains a space survives, which one of these does: a remapping
/// under a home directory whose name has a space in it.
const ENCODED_VARIABLE: &str = "CARGO_ENCODED_RUSTFLAGS";

/// The variable that names cargo's target directory.
const TARGET_DIRECTORY_VARIABLE: &str = "CARGO_TARGET_DIR";

/// The variable it is preferred to, removed so that nothing of the caller's
/// leaks into a build whose flags are meant to be exactly these.
const FLAGS_VARIABLE: &str = "RUSTFLAGS";

/// What separates them in the encoded form.
const ENCODED_SEPARATOR: char = '\u{1f}';

/// Builds the binary for `target` and says where cargo put it, with cargo's
/// progress captured or shown as `output` says.
///
/// # Errors
///
/// [`DistributionError::Build`] when the build fails or its toolchain is
/// absent, naming what cargo said.
pub fn build(root: &Path, target: &str, output: Output) -> Result<PathBuf, DistributionError> {
    let target_directory = crate::distribution::target_directory(root);
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
        // The directory this reads the binary back from, said rather than
        // left for cargo to find again in a configuration.
        .env(TARGET_DIRECTORY_VARIABLE, &target_directory)
        .env(ENCODED_VARIABLE, rustflags(root, target)?)
        // Cargo would prefer the encoded form anyway; removing this says so
        // rather than leaving a caller's value to look as though it applied.
        .env_remove(FLAGS_VARIABLE);
    // Its output is cargo's own progress: captured for a release, where what
    // matters is that it succeeded, and shown to a person waiting on it.
    process::run(command, Deadline(BUILD_DEADLINE), output).map_err(|source| {
        DistributionError::Build {
            target: target.to_owned(),
            detail: source.to_string(),
        }
    })?;
    Ok(built_at(&target_directory, target))
}

/// The flags this build needs: what the workspace configures for the target,
/// and the remapping that makes two builds of one commit agree.
///
/// Encoded, not joined by spaces: a remapping under a home directory with a
/// space in its name is one flag, and whitespace-splitting would make it two
/// and fail the build on a machine whose only sin was its owner's name.
///
/// Either form replaces every configured flag rather than adding to them, so
/// the configured ones are read and passed on. Without that, the musl targets
/// lose `link-self-contained=no` and link rustc's startup objects beside the
/// cross-compiler's, which is a duplicate `_start` and nothing else.
///
/// # Errors
///
/// [`DistributionError::Unreadable`] when `.cargo/config.toml` is there but
/// cannot be read or parsed. Falling back to no flags is the one thing this
/// must not do: what is set replaces the configured flags rather than adding
/// to them, so an empty answer silently drops `link-self-contained=no` and the
/// musl link dies on a duplicate `_start` — the failure this exists to
/// prevent.
pub fn rustflags(root: &Path, target: &str) -> Result<String, DistributionError> {
    let mut flags = configured(root, target)?;
    flags.push(format!("--remap-path-prefix={}={REMAPPED}", root.display()));
    // Two builds of one commit must agree across machines, not only across
    // runs on one. A dependency's panic locations carry the absolute path of
    // the registry it was unpacked into, which lives under a home directory
    // whose name differs for every person who builds this.
    if let Some(home) = cargo_home() {
        flags.push(format!(
            "--remap-path-prefix={}={REMAPPED_REGISTRY}",
            home.join(REGISTRY_SOURCES).display()
        ));
        flags.push(format!(
            "--remap-path-prefix={}={REMAPPED_CHECKOUTS}",
            home.join(GIT_CHECKOUTS).display()
        ));
    }
    Ok(flags.join(&ENCODED_SEPARATOR.to_string()))
}

/// Where cargo keeps what it has downloaded: what `CARGO_HOME` names, or
/// `.cargo` under the home directory.
fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

/// The flags `.cargo/config.toml` sets for a target, and for every build.
///
/// # Errors
///
/// [`DistributionError::Unreadable`] when the file is there and cannot be
/// read or parsed. A workspace with no such file has no configured flags,
/// which is a different thing and not an error.
fn configured(root: &Path, target: &str) -> Result<Vec<String>, DistributionError> {
    let path = root.join(CARGO_CONFIG);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|source| DistributionError::Io {
        path: path.clone(),
        source,
    })?;
    let config =
        text.parse::<toml::Table>()
            .map_err(|_unparsed| DistributionError::Unreadable {
                path,
                wanted: "a table of build and target flags".to_owned(),
            })?;
    let build = config.get("build").and_then(|table| table.get("rustflags"));
    let targeted = config
        .get("target")
        .and_then(|table| table.get(target))
        .and_then(|table| table.get("rustflags"));
    Ok([build, targeted]
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_array)
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// Where cargo puts the binary for a target under the release profile, in
/// the target directory the build was given.
#[must_use]
pub fn built_at(target_directory: &Path, target: &str) -> PathBuf {
    target_directory.join(target).join(PROFILE).join(BINARY)
}
