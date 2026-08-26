//! The length check against a repository of this test's making and the real
//! tree: a 1001-line file is reported and a 1000-line file is not,
//! `Cargo.lock` is never reported, untracked-but-not-ignored files are
//! counted, and ignored files are not.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{Deadline, Output, run};
use xtask::policy::length::{MAXIMUM_LINES, check};
use xtask::repository_root;

/// How long a git command in the scratch repository may take.
const GIT_DEADLINE: Duration = Duration::from_secs(20);

/// A scratch git repository, removed when dropped.
#[derive(Debug)]
struct Repository {
    /// Its directory.
    path: PathBuf,
}

impl Repository {
    /// Creates and initializes the repository.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created or git cannot initialize it.
    fn new() -> Result<Repository, String> {
        let path = env::temp_dir().join(format!("iznik-policy-length-{}", std::process::id()));
        fs::create_dir_all(&path)
            .map_err(|error| format!("creating {}: {error}", path.display()))?;
        let repository = Repository { path };
        repository.git(&["init", "--quiet"])?;
        Ok(repository)
    }

    /// Runs git inside the repository.
    ///
    /// # Errors
    ///
    /// When git fails.
    fn git(&self, arguments: &[&str]) -> Result<(), String> {
        let mut command = Command::new("git");
        command.args(arguments).current_dir(&self.path);
        run(command, Deadline(GIT_DEADLINE), Output::Capture)
            .map(|_completed| ())
            .map_err(|error| error.to_string())
    }

    /// Writes a file of `lines` lines.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    fn write_lines(&self, name: &str, lines: usize) -> Result<(), String> {
        let text: String = (0..lines).map(|_line| "x\n").collect();
        fs::write(self.path.join(name), text).map_err(|error| format!("writing {name}: {error}"))
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap_or_default();
    }
}

/// A file over the limit is reported, tracked or untracked; a file at the
/// limit, `Cargo.lock` and an ignored file are not.
///
/// # Panics
///
/// When the report differs.
#[test]
fn policy_length_reports_files_over_the_limit() {
    let repository = Repository::new().expect("a scratch repository");
    let over = MAXIMUM_LINES.checked_add(1).expect("a count");
    repository
        .write_lines("long.txt", over)
        .expect("a long file");
    repository
        .write_lines("limit.txt", MAXIMUM_LINES)
        .expect("a file at the limit");
    repository
        .write_lines("Cargo.lock", over)
        .expect("a long lockfile");
    repository
        .write_lines("untracked.txt", over)
        .expect("a long untracked file");
    repository
        .write_lines("ignored.txt", over)
        .expect("a long ignored file");
    fs::write(repository.path.join(".gitignore"), "ignored.txt\n").expect("an ignore file");
    repository
        .git(&["add", "long.txt", "limit.txt", "Cargo.lock", ".gitignore"])
        .expect("staging");
    let violations = check(&repository.path).expect("the check runs");
    let mut reported: Vec<&Path> = violations
        .iter()
        .map(|violation| violation.path.as_path())
        .collect();
    reported.sort();
    assert_eq!(
        reported,
        [Path::new("long.txt"), Path::new("untracked.txt")],
        "the files over the limit: {violations:?}"
    );
    let long = violations
        .iter()
        .find(|violation| violation.path == Path::new("long.txt"))
        .expect("the long file");
    assert_eq!(long.rule, "length", "the rule");
    assert!(
        long.detail.contains(&format!("{over} lines")),
        "the detail counts the lines: {}",
        long.detail
    );
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_length_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
