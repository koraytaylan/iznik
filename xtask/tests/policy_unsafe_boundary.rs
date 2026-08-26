//! The unsafe boundary check against its synthetic trees and the real tree:
//! a crate root without `forbid(unsafe_code)` is reported — a binary's
//! `main.rs` included — unless it is `iznik-ffi`'s.

use std::path::{Path, PathBuf};

use xtask::policy::fixture_root;
use xtask::policy::unsafe_boundary::check;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "unsafe_boundary", tree)
}

/// Both roots of the crate without the attribute are reported; the boundary
/// crate and the crate with it are not.
///
/// # Panics
///
/// When the report differs.
#[test]
fn policy_unsafe_boundary_reports_roots_without_the_attribute() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let mut reported: Vec<&Path> = violations
        .iter()
        .map(|violation| violation.path.as_path())
        .collect();
    reported.sort();
    assert_eq!(
        reported,
        [
            Path::new("crates/alpha/src/lib.rs"),
            Path::new("crates/alpha/src/main.rs")
        ],
        "the roots without the attribute: {violations:?}"
    );
    assert!(
        violations
            .iter()
            .all(|violation| violation.rule == "unsafe-boundary"),
        "the rule: {violations:?}"
    );
}

/// The clean sibling has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_unsafe_boundary_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_unsafe_boundary_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
