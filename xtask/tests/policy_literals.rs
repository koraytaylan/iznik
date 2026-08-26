//! The literals check against its synthetic trees and the real tree: `0` and
//! `1` pass; `2` in an expression fails with its line; any value in a `const`
//! or `static` initializer or an enum discriminant passes; an array repeat
//! count, an array-type length, a literal handed to a macro and a literal in
//! a pattern are checked;
//! a literal in a `tests/` file, a `benches/` file or under the fixtures is
//! not reported.

use std::path::{Path, PathBuf};

use xtask::policy::fixture_root;
use xtask::policy::literals::check;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "literals", tree)
}

/// Every literal position the rule names, and none it exempts.
///
/// # Panics
///
/// When the reported lines and values differ.
#[test]
fn policy_literals_reports_every_magic_number_and_nothing_else() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let reported: Vec<(&Path, Option<usize>, &str)> = violations
        .iter()
        .map(|violation| {
            let value = violation
                .detail
                .split('`')
                .nth(1)
                .expect("the detail names the value");
            (violation.path.as_path(), violation.line, value)
        })
        .collect();
    let lib = Path::new("crates/example/src/lib.rs");
    let expected: Vec<(&Path, Option<usize>, &str)> = vec![
        (lib, Some(13), "2"),
        (lib, Some(14), "4"),
        (lib, Some(15), "3"),
        (lib, Some(16), "5"),
        (lib, Some(17), "2.5"),
        (lib, Some(22), "2"),
    ];
    assert_eq!(reported, expected, "the magic numbers, in order");
}

/// The clean sibling has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_literals_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_literals_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
