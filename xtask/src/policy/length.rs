//! No file over a thousand lines, whatever its kind — the synthetic trees
//! under the fixtures included — `Cargo.lock` excepted: every file git tracks
//! or would track — untracked but not ignored — is counted. A file that is
//! not text has no lines and is not counted.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};

use super::{PolicyError, Violation, relative};

/// The most lines a file may have.
pub const MAXIMUM_LINES: usize = 1000;

/// The one generated file the rule does not apply to.
const EXEMPT_FILE: &str = "Cargo.lock";

/// The rule a long file breaks.
const RULE: &str = "length";

/// How long `git ls-files` may take: a minute is far beyond a listing of any
/// repository this size, and a bound on a hung git.
const GIT_DEADLINE: Duration = Duration::from_mins(1);

/// The files git tracks or would track under `root`, relative to it.
///
/// # Errors
///
/// [`PolicyError::Process`] when git cannot list them.
fn listed_files(root: &Path) -> Result<Vec<String>, PolicyError> {
    let mut command = Command::new("git");
    command
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root);
    let completed = process::run(command, Deadline(GIT_DEADLINE), Output::Capture)
        .map_err(PolicyError::Process)?;
    Ok(String::from_utf8_lossy(&completed.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The length check.
///
/// # Errors
///
/// [`PolicyError::Process`] when git cannot list the files, and
/// [`PolicyError::Io`] when a listed file cannot be read.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    for listed in listed_files(root)? {
        let path = root.join(&listed);
        let is_exempt = path.file_name().is_some_and(|name| name == EXEMPT_FILE);
        if is_exempt || !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|source| PolicyError::Io {
            path: path.clone(),
            source,
        })?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let lines = text.lines().count();
        if lines > MAXIMUM_LINES {
            violations.push(Violation {
                path: relative(root, &path),
                line: None,
                rule: RULE,
                detail: format!("{lines} lines; the most a file may have is {MAXIMUM_LINES}"),
            });
        }
    }
    Ok(violations)
}
