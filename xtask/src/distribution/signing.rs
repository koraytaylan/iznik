//! Signing a macOS application bundle, and checking the signature holds.
//!
//! With `IZNIK_CODESIGN_IDENTITY` set, the bundle is signed with that
//! identity — a Developer ID certificate in a keychain `codesign` can reach —
//! under the hardened runtime and with a secure timestamp, which is what
//! notarization asks for. Without it the bundle is signed ad hoc: no
//! identity, so Gatekeeper still asks a person who downloaded it to allow it,
//! but its contents are sealed, and an Apple silicon Mac runs nothing
//! unsigned.
//!
//! Inside out: every macOS server the bundle carries under
//! `Contents/Resources` is signed on its own first, because `--deep` signs
//! only the nested code in the places code belongs and notarization rejects
//! an unsigned Mach-O anywhere. Then the bundle, then `codesign --verify
//! --deep --strict` over the whole of it. The servers' new signatures change
//! their bytes; nothing reads a digest recorded before, because the bootstrap
//! computes the digest of what it uploads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};

use crate::distribution::app::DARWIN_SUFFIX;
use crate::distribution::{BINARY, DistributionError};

/// The variable naming the signing identity; unset or empty signs ad hoc.
pub const IDENTITY_VARIABLE: &str = "IZNIK_CODESIGN_IDENTITY";

/// The identity `codesign` reads as "ad hoc".
const AD_HOC: &str = "-";

/// The program that signs and verifies.
const CODESIGN: &str = "codesign";

/// How long one signature or verification may take: a secure timestamp is a
/// round trip to Apple, and a bundle of servers is tens of megabytes to hash.
const SIGNING_DEADLINE: Duration = Duration::from_mins(5);

/// Where a macOS bundle keeps the servers it carries, one directory per
/// triple.
const SERVERS: &[&str] = &["Contents", "Resources", "artifacts"];

/// How a bundle is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// With no identity.
    AdHoc,
    /// With a named identity, the hardened runtime and a secure timestamp.
    Identity(String),
}

impl Signature {
    /// The signature `IZNIK_CODESIGN_IDENTITY` asks for.
    #[must_use]
    pub fn from_environment() -> Signature {
        Signature::named(std::env::var(IDENTITY_VARIABLE).ok().as_deref())
    }

    /// The signature an identity asks for: none, an empty one or `-` is ad
    /// hoc, and anything else is that identity.
    #[must_use]
    pub fn named(identity: Option<&str>) -> Signature {
        match identity.map(str::trim) {
            Some(named) if !named.is_empty() && named != AD_HOC => {
                Signature::Identity(named.to_owned())
            }
            _ => Signature::AdHoc,
        }
    }

    /// The arguments `codesign` signs with.
    fn arguments(&self) -> Vec<String> {
        let mut arguments = vec!["--force".to_owned(), "--sign".to_owned()];
        match self {
            Signature::AdHoc => arguments.push(AD_HOC.to_owned()),
            Signature::Identity(identity) => arguments.extend([
                identity.clone(),
                "--options".to_owned(),
                "runtime".to_owned(),
                "--timestamp".to_owned(),
            ]),
        }
        arguments
    }
}

/// Signs the macOS bundle at `bundle` — its macOS servers, then itself — and
/// verifies the result.
///
/// # Errors
///
/// [`DistributionError::Build`] naming `target` when `codesign` cannot be
/// run, refuses to sign, or finds the signed bundle does not verify.
pub fn sign_bundle(
    bundle: &Path,
    target: &str,
    signature: &Signature,
) -> Result<(), DistributionError> {
    for server in darwin_servers(bundle) {
        codesign(target, signature.arguments(), &server)?;
    }
    let mut arguments = signature.arguments();
    arguments.insert(1, "--deep".to_owned());
    codesign(target, arguments, bundle)?;
    codesign(
        target,
        vec![
            "--verify".to_owned(),
            "--deep".to_owned(),
            "--strict".to_owned(),
        ],
        bundle,
    )
}

/// The macOS servers a bundle carries, sorted.
fn darwin_servers(bundle: &Path) -> Vec<PathBuf> {
    let directory = SERVERS
        .iter()
        .fold(bundle.to_path_buf(), |path, segment| path.join(segment));
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(DARWIN_SUFFIX))
        .map(|entry| entry.path().join(BINARY))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found
}

/// Runs `codesign` with `arguments` over `path`.
///
/// # Errors
///
/// [`DistributionError::Build`] naming `target` with what `codesign` said.
fn codesign(target: &str, arguments: Vec<String>, path: &Path) -> Result<(), DistributionError> {
    let mut command = Command::new(CODESIGN);
    command.args(arguments).arg(path);
    process::run(command, Deadline(SIGNING_DEADLINE), Output::Capture)
        .map(|_completed| ())
        .map_err(|error| DistributionError::Build {
            target: target.to_owned(),
            detail: format!("signing {}: {error}", path.display()),
        })
}
