//! The links check against its synthetic trees and the real tree: a relative
//! link or reference target to a missing file is reported with its file and
//! line; links with a scheme, a bare fragment, a fragment on an existing
//! file, inline code and fenced blocks are not.

use std::path::{Path, PathBuf};

use xtask::policy::fixture_root;
use xtask::policy::links::check;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "links", tree)
}

/// A dangling inline link and a dangling reference definition are reported
/// with their lines; the link that resolves is not.
///
/// # Panics
///
/// When the report differs.
#[test]
fn policy_links_reports_dangling_targets_with_their_lines() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let reported: Vec<(&Path, Option<usize>)> = violations
        .iter()
        .map(|violation| (violation.path.as_path(), violation.line))
        .collect();
    assert_eq!(
        reported,
        [
            (Path::new("README.md"), Some(3)),
            (Path::new("README.md"), Some(5)),
            (Path::new("README.md"), Some(7))
        ],
        "the dangling targets: {violations:?}"
    );
    assert!(
        violations
            .iter()
            .any(|violation| violation.detail.contains("`missing.md`")),
        "the inline target is named: {violations:?}"
    );
    assert!(
        violations
            .iter()
            .any(|violation| violation.detail.contains("`also-missing.md`")),
        "the reference target is named: {violations:?}"
    );
}

/// The clean sibling — schemes, fragments, a fragment on an existing file,
/// inline code and a fenced block — has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_links_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_links_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
