//! Which tasks a run verifies: explicit ids, every task with a claims file, or
//! the current branch's diff under the rule that gives the registry its teeth.
//!
//! `Selection::CurrentBranch` is the gate's selection. It diffs the working
//! tree, untracked files included, against its merge base with the trunk and
//! takes the tasks whose claims changed; and if that diff touches product or
//! tooling code while changing no claims, and a registry exists, it fails —
//! that is exactly the case the registry exists to catch.
//!
//! When the merge base is `HEAD` itself — on the trunk, or on a branch with no
//! commit of its own — that diff would be only the uncommitted work, and a
//! commit made on the trunk would be checked by nothing. There the base is
//! `HEAD`'s parent instead, so the selection covers the working tree plus the
//! last commit. `coverage` remains the run that proves the whole tree.
//!
//! A claims file counts as changed only when what it declares changed: one
//! whose TOML parses to the same table before and after — a comment, a
//! reordered key, whitespace — neither selects its task nor satisfies the
//! product-code rule, because it declares nothing new.

use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};

use crate::claims::TaskId;
use crate::claims::registry::CLAIMS_DIRECTORY;

/// The branch every task's merge base is taken against.
const TRUNK: &str = "develop";

/// How long a `git` query may take before it is a failure, not a wait.
const GIT_DEADLINE: Duration = Duration::from_secs(30);

/// The prefix of a claims file's path, under the repository root.
const CLAIMS_PREFIX: &str = "regression/claims/";

/// A claims file's extension, with its dot.
const CLAIMS_SUFFIX: &str = ".toml";

/// Which tasks to verify.
#[derive(Clone, Debug)]
pub enum Selection {
    /// Exactly these task ids.
    Tasks(Vec<TaskId>),
    /// Every task that has a claims file.
    Everything,
    /// The tasks whose claims files the branch changed, under the product-code
    /// rule.
    CurrentBranch,
}

/// Why a selection could not be made.
#[derive(Debug)]
pub enum SelectionError {
    /// A `git` query failed.
    Git {
        /// What went wrong.
        detail: String,
    },
    /// The branch changes product or tooling code but declares no claims.
    ProductCodeWithoutClaims {
        /// The product-or-tooling paths the diff touched.
        paths: Vec<String>,
    },
}

impl Display for SelectionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SelectionError::Git { detail } => write!(formatter, "git: {detail}"),
            SelectionError::ProductCodeWithoutClaims { paths } => write!(
                formatter,
                "the branch changes code but declares no claims: {}",
                paths.join(", ")
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

/// The task ids a selection resolves to, sorted and unique.
///
/// # Errors
///
/// [`SelectionError::Git`] when a `git` query fails, and
/// [`SelectionError::ProductCodeWithoutClaims`] when the branch changes code
/// without declaring claims.
pub fn select(root: &Path, selection: &Selection) -> Result<Vec<TaskId>, SelectionError> {
    let tasks = match selection {
        Selection::Tasks(ids) => ids.clone(),
        Selection::Everything => tasks_with_a_claims_file(root),
        Selection::CurrentBranch => current_branch(root)?,
    };
    Ok(sorted_unique(tasks))
}

/// Every task that has a claims file directly under `regression/claims/`.
fn tasks_with_a_claims_file(root: &Path) -> Vec<TaskId> {
    let Ok(entries) = std::fs::read_dir(root.join(CLAIMS_DIRECTORY)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect()
}

/// The tasks the branch's diff selects, after the product-code rule.
///
/// # Errors
///
/// [`SelectionError`] when a `git` query fails or the rule is broken.
fn current_branch(root: &Path) -> Result<Vec<TaskId>, SelectionError> {
    let base = comparison_base(root)?;
    let paths = changed_paths(root, &base)?;
    let claims_tasks: Vec<TaskId> = paths
        .iter()
        .filter_map(|path| claims_file_task(path))
        .filter(|task| declares_something_new(root, &base, task))
        .collect();
    let product: Vec<String> = paths
        .iter()
        .filter(|path| is_product_code(path))
        .cloned()
        .collect();
    let registry_exists = root.join(CLAIMS_DIRECTORY).is_dir();
    if registry_exists && !product.is_empty() && claims_tasks.is_empty() {
        return Err(SelectionError::ProductCodeWithoutClaims { paths: product });
    }
    Ok(claims_tasks)
}

/// The commit the working tree is compared with: the merge base with the
/// trunk, or — when that is `HEAD` itself — `HEAD`'s parent, or the empty
/// tree when `HEAD` has none.
///
/// # Errors
///
/// [`SelectionError::Git`] when a `git` query fails.
fn comparison_base(root: &Path) -> Result<String, SelectionError> {
    let head = git(root, &["rev-parse", "HEAD"])?;
    let base = git(root, &["merge-base", TRUNK, "HEAD"])?;
    if base.trim() != head.trim() {
        return Ok(base.trim().to_owned());
    }
    match git(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"]) {
        Ok(parent) if !parent.trim().is_empty() => Ok(parent.trim().to_owned()),
        Ok(_) | Err(_) => {
            let empty = git(root, &["hash-object", "-t", "tree", "/dev/null"])?;
            Ok(empty.trim().to_owned())
        }
    }
}

/// Every path the working tree changes against `base`, tracked or not.
///
/// # Errors
///
/// [`SelectionError::Git`] when a `git` query fails.
fn changed_paths(root: &Path, base: &str) -> Result<Vec<String>, SelectionError> {
    let changed = git(root, &["diff", "--name-only", base])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard"])?;
    let paths: BTreeSet<String> = changed
        .lines()
        .chain(untracked.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(paths.into_iter().collect())
}

/// Whether a task's claims file declares something other than it did at
/// `base`: it was added or removed, or its TOML no longer parses to the same
/// table. A file that fails to parse on either side counts as changed, so
/// the registry's own loading reports it.
fn declares_something_new(root: &Path, base: &str, task: &str) -> bool {
    let relative = format!("{CLAIMS_PREFIX}{task}{CLAIMS_SUFFIX}");
    let before = git(root, &["show", &format!("{base}:{relative}")]).ok();
    let after = std::fs::read_to_string(root.join(&relative)).ok();
    match (before, after) {
        (None, None) => false,
        (Some(before), Some(after)) => {
            match (before.parse::<toml::Table>(), after.parse::<toml::Table>()) {
                (Ok(before), Ok(after)) => before != after,
                _ => true,
            }
        }
        _ => true,
    }
}

/// The task a claims-file path names, if the path is a claims file.
fn claims_file_task(path: &str) -> Option<TaskId> {
    path.strip_prefix(CLAIMS_PREFIX)
        .and_then(|name| name.strip_suffix(CLAIMS_SUFFIX))
        .filter(|task| !task.contains('/'))
        .map(str::to_owned)
}

/// Whether a path is product or tooling code: under a crate's `src/` or
/// `benches/`, or under `xtask/src/`.
fn is_product_code(path: &str) -> bool {
    if let Some(rest) = path.strip_prefix("crates/") {
        return rest.split_once('/').is_some_and(|(_crate, tail)| {
            tail.starts_with("src/") || tail.starts_with("benches/")
        });
    }
    path.starts_with("xtask/src/")
}

/// Runs a `git` query in `root` and returns its standard output.
///
/// # Errors
///
/// [`SelectionError::Git`] when `git` cannot be run or exits non-zero.
fn git(root: &Path, arguments: &[&str]) -> Result<String, SelectionError> {
    let mut command = Command::new("git");
    command.current_dir(root).args(arguments);
    let completed =
        process::run(command, Deadline(GIT_DEADLINE), Output::Capture).map_err(|source| {
            SelectionError::Git {
                detail: source.to_string(),
            }
        })?;
    Ok(String::from_utf8_lossy(&completed.stdout).into_owned())
}

/// A sorted, unique copy of a list of task ids.
fn sorted_unique(tasks: Vec<TaskId>) -> Vec<TaskId> {
    let ordered: BTreeSet<TaskId> = tasks.into_iter().collect();
    ordered.into_iter().collect()
}
