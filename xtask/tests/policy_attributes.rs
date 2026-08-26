//! The attributes check against its synthetic trees and the real tree: inner
//! and outer `allow`, `expect` and `cfg(test)` attributes are reported
//! anywhere under `crates/` and `xtask/`, tests included, and not under the
//! fixtures.

use std::path::{Path, PathBuf};

use xtask::policy::attributes::check;
use xtask::policy::fixture_root;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "attributes", tree)
}

/// Every waiver and every `cfg(test)` is reported with its rule and line, in
/// sources and tests alike; a `cfg` that is not `test` and a file under the
/// fixtures are not.
///
/// # Panics
///
/// When the report differs.
#[test]
fn policy_attributes_reports_waivers_and_test_configuration() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let reported: Vec<(&Path, Option<usize>, &str)> = violations
        .iter()
        .map(|violation| (violation.path.as_path(), violation.line, violation.rule))
        .collect();
    let lib = Path::new("crates/example/src/lib.rs");
    let expected: Vec<(&Path, Option<usize>, &str)> = vec![
        (lib, Some(2), "attribute-waiver"),
        (lib, Some(3), "attribute-waiver"),
        (lib, Some(5), "attribute-waiver"),
        (lib, Some(7), "attribute-cfg-test"),
        (lib, Some(9), "attribute-cfg-test"),
        (lib, Some(13), "attribute-cfg-test"),
        (lib, Some(13), "attribute-waiver"),
        (
            Path::new("crates/example/tests/cases.rs"),
            Some(2),
            "attribute-waiver",
        ),
    ];
    assert_eq!(
        reported, expected,
        "the attributes, in order: {violations:?}"
    );
}

/// The clean sibling has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_attributes_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_attributes_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
