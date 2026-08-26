//! The claims registry: what a task claims about runtime behavior, the proof that establishes each claim, and the gate that runs the proofs.
//!
//! Filled by task `claims-registry` of plan 0001; until then this module holds only its documentation and the stub of its entry point.

pub mod registry;
pub mod selection;
pub mod verify;

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

/// The directory the claims registry lives in, relative to the repository root.
const CLAIMS_DIRECTORY: &str = "regression/claims";

/// The entry point of `xtask claims verify` and `xtask claims coverage`; a
/// stub until task `claims-registry` replaces its body.
///
/// `verify` is the fifth gate, so its stub is the gate's bootstrap form rather
/// than a refusal: it exits 0 while `regression/claims/` does not exist under
/// the root and fails naming the task once it does, so the gate passes on
/// every task that lands before the registry and cannot be forgotten by the
/// task that lands it. `--root <directory>` names a root other than this
/// repository, as the real subcommand will accept it. `coverage` is a plain
/// stub.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.get(1).and_then(|argument| argument.to_str()) == Some("verify") {
        return verify_bootstrap(arguments);
    }
    writeln!(
        std::io::stderr(),
        "claims: not implemented until task claims-registry"
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}

/// Passes while there is no registry to verify; fails naming the task once
/// there is one.
fn verify_bootstrap(arguments: &[OsString]) -> ExitCode {
    let root = root_argument(arguments).unwrap_or_else(crate::repository_root);
    let claims = root.join(CLAIMS_DIRECTORY);
    if claims.is_dir() {
        writeln!(
            std::io::stderr(),
            "claims verify: {} exists, and verifying it is not implemented until task claims-registry",
            claims.display()
        )
        .unwrap_or_default();
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// The value following `--root`, when the command line carries one.
fn root_argument(arguments: &[OsString]) -> Option<PathBuf> {
    let mut iterator = arguments.iter();
    iterator
        .find(|argument| argument.as_os_str() == "--root")
        .and_then(|_flag| iterator.next())
        .map(PathBuf::from)
}
