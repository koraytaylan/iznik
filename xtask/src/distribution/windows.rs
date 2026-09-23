//! The Windows triples, which are refused rather than built.
//!
//! A Windows server is not distributed, for two reasons. Its daemon listens on
//! a loopback TCP port with no authentication, so any local user of the host
//! could connect to it and drive every shell it holds; on Unix the same
//! daemon listens on a socket only its owner can open. And the client cannot
//! start it: the relay command a channel sends is quoted for a POSIX shell,
//! which `cmd.exe` does not read. Until both change, `xtask distribution`
//! names these triples and says why, rather than calling them unknown or
//! producing an artifact nothing should install.

use std::path::{Path, PathBuf};

use iznik_harness::process::Output;

use crate::distribution::DistributionError;

/// The triples this refuses, named so that asking for one gets the reason
/// rather than "not a target this builds for".
pub const TARGETS: &[&str] = &["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];

/// Why a Windows server is not built, as the refusal says it.
pub const REFUSAL: &str = "a Windows server is not distributed: its daemon listens on \
     loopback TCP without authentication, so any local user of the host could \
     reach every shell it holds, and the client cannot start it, because the \
     command it sends is quoted for a POSIX shell. Install a Linux or macOS \
     server instead; the Windows client itself is still built.";

/// Refuses a Windows `target`, with [`REFUSAL`] as the reason.
///
/// # Errors
///
/// Always [`DistributionError::Build`].
pub fn build(_root: &Path, target: &str, _output: Output) -> Result<PathBuf, DistributionError> {
    Err(DistributionError::Build {
        target: target.to_owned(),
        detail: REFUSAL.to_owned(),
    })
}
