//! `xtask app-bundle`: invoke the product's headless bundle writer.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitCode;

/// Target flag.
const TARGET_FLAG: &str = "--target";
/// Input binary flag.
const BINARY_FLAG: &str = "--binary";
/// Output directory flag.
const OUTPUT_FLAG: &str = "--output";
/// Usage exit status.
const USAGE_EXIT_CODE: u8 = 2;
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
        .status();
    match status {
        Ok(result) if result.success() => ExitCode::SUCCESS,
        Ok(_) | Err(_) => ExitCode::FAILURE,
    }
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
    let _written = writeln!(
        std::io::stderr(),
        "usage: xtask app-bundle --target <triple> --binary <path> --output <path>"
    );
    ExitCode::from(USAGE_EXIT_CODE)
}

/// Report app-bundle usage successfully for `--help`.
fn usage_success() -> ExitCode {
    let _written = writeln!(
        std::io::stdout(),
        "usage: xtask app-bundle --target <triple> --binary <path> --output <path>"
    );
    ExitCode::SUCCESS
}
