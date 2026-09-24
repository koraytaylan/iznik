//! `xtask app-bundle`: invoke the product's headless bundle writer, then write
//! the bundle's `THIRD-PARTY-NOTICES`, then, for a macOS target, sign the
//! bundle as [`signing`] describes.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::ExitCode;

use crate::distribution::signing::{self, Signature};
use crate::distribution::{BINARY, DistributionError, notices};

/// Target flag.
const TARGET_FLAG: &str = "--target";
/// Input binary flag.
const BINARY_FLAG: &str = "--binary";
/// Output directory flag.
const OUTPUT_FLAG: &str = "--output";
/// The flag naming the directory of servers the bundle carries, laid out as
/// `xtask distribution` lays them out: one `<triple>/iznik-server` each.
const SERVERS_FLAG: &str = "--servers";
/// The usage line, shared by the refusal and `--help`.
const USAGE_LINE: &str =
    "usage: xtask app-bundle --target <triple> --binary <path> --output <path> --servers <path>";
/// Usage exit status.
const USAGE_EXIT_CODE: u8 = 2;
/// The package the bundled executable is built from.
const APPLICATION_PACKAGE: &str = "iznik-app";
/// What a macOS target's triple ends in.
pub const DARWIN_SUFFIX: &str = "apple-darwin";
/// Where a macOS bundle keeps its resources, under the bundle.
const DARWIN_RESOURCES: &[&str] = &["Contents", "Resources"];
/// Number of items needed to read a flag and its value.
const FLAG_WINDOW_LENGTH: usize = 2;

/// Run the app bundle command.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.iter().any(|argument| argument == "--help") {
        return usage_success();
    }
    let Some(target) = value(arguments, TARGET_FLAG) else {
        return usage();
    };
    let Some(binary) = value(arguments, BINARY_FLAG) else {
        return usage();
    };
    let Some(output) = value(arguments, OUTPUT_FLAG) else {
        return usage();
    };
    let Some(servers) = value(arguments, SERVERS_FLAG) else {
        return usage();
    };
    let servers_path = PathBuf::from(servers);
    if !servers_path.is_dir() {
        let _written = writeln!(
            std::io::stderr(),
            "app-bundle: servers directory does not exist: {}",
            servers_path.display()
        );
        return ExitCode::FAILURE;
    }
    let binary_path = PathBuf::from(binary);
    if !binary_path.is_file() {
        let _written = writeln!(
            std::io::stderr(),
            "app-bundle: binary does not exist: {}",
            binary_path.display()
        );
        return ExitCode::FAILURE;
    }
    let version = env!("CARGO_PKG_VERSION");
    let status = Command::new("cargo")
        .args(["run", "--locked", "--package", "iznik-app", "--quiet", "--"])
        .arg("--bundle")
        .arg(&target)
        .arg(&binary_path)
        .arg(&output)
        .arg(version)
        .arg(&servers_path)
        .status();
    if !status.is_ok_and(|result| result.success()) {
        return ExitCode::FAILURE;
    }
    match finish(&target, &servers_path, Path::new(&output)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _written = writeln!(std::io::stderr(), "app-bundle: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What follows the bundle writer: the notices, and for a macOS target the
/// signature over everything, which must come last because it seals the
/// bundle's resources.
///
/// # Errors
///
/// What [`bundle_notices`] and [`signing::sign_bundle`] report.
fn finish(target: &str, servers: &Path, output: &Path) -> Result<(), DistributionError> {
    let _written = bundle_notices(
        &crate::distribution::workspace_root(),
        target,
        servers,
        output,
    )?;
    if target.ends_with(DARWIN_SUFFIX) {
        let signature = Signature::from_environment();
        signing::sign_bundle(output, target, &signature)?;
        let how = match &signature {
            Signature::AdHoc => format!(
                "ad hoc, with no identity; set {} to sign with one",
                signing::IDENTITY_VARIABLE
            ),
            Signature::Identity(identity) => format!("with {identity}"),
        };
        let _said = writeln!(std::io::stdout(), "app-bundle: signed {how}");
    }
    Ok(())
}

/// Writes `THIRD-PARTY-NOTICES` into a bundle: the packages the application is
/// built from for `target`, and those of every server the bundle carries for
/// its own triple. A macOS bundle keeps it under `Contents/Resources`; the
/// other layouts at their root. Says where it wrote it.
///
/// # Errors
///
/// What [`notices::write`] reports.
pub fn bundle_notices(
    root: &Path,
    target: &str,
    servers: &Path,
    output: &Path,
) -> Result<PathBuf, DistributionError> {
    let triples: Vec<String> = std::fs::read_dir(servers)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join(BINARY).is_file())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let mut roots = vec![(APPLICATION_PACKAGE, target)];
    roots.extend(triples.iter().map(|triple| (BINARY, triple.as_str())));
    let directory = if target.ends_with(DARWIN_SUFFIX) {
        DARWIN_RESOURCES
            .iter()
            .fold(output.to_path_buf(), |path, segment| path.join(segment))
    } else {
        output.to_path_buf()
    };
    let path = directory.join(notices::NOTICES);
    notices::write(
        root,
        &format!("the iznik application for {target}"),
        &roots,
        &path,
    )?;
    Ok(path)
}

/// Read a string value following a flag.
fn value(arguments: &[OsString], flag: &str) -> Option<String> {
    arguments
        .windows(FLAG_WINDOW_LENGTH)
        .find(|pair| pair.first().is_some_and(|value| value == flag))
        .and_then(|pair| pair.get(1))
        .and_then(|value| value.to_str())
        .map(str::to_owned)
}

/// Report app-bundle usage.
fn usage() -> ExitCode {
    let _written = writeln!(std::io::stderr(), "{USAGE_LINE}");
    ExitCode::from(USAGE_EXIT_CODE)
}

/// Report app-bundle usage successfully for `--help`.
fn usage_success() -> ExitCode {
    let _written = writeln!(std::io::stdout(), "{USAGE_LINE}");
    ExitCode::SUCCESS
}
