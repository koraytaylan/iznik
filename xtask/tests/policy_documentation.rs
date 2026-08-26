//! The documentation check against its synthetic trees and the real tree: a
//! crate root without the README include, a crate without a README, a
//! README without the crate heading, a source file without a leading `//!`,
//! a module absent from its
//! crate's README, and a fence in a README or a doc comment that is untagged
//! or tagged `rust` are each reported.

use std::path::{Path, PathBuf};

use xtask::policy::Violation;
use xtask::policy::documentation::check;
use xtask::policy::fixture_root;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "documentation", tree)
}

/// Whether a violation is of a rule at a path and line.
fn is_at(violation: &Violation, rule: &str, path: &str, line: Option<usize>) -> bool {
    violation.rule == rule && violation.path == Path::new(path) && violation.line == line
}

/// Each rule is reported where the fixture breaks it, and nowhere else.
///
/// # Panics
///
/// When a rule is not reported where it is broken, or the count differs.
#[test]
fn policy_documentation_reports_each_rule_where_it_is_broken() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let expected: &[(&str, &str, Option<usize>)] = &[
        ("documentation-include", "crates/alpha/src/lib.rs", None),
        ("documentation-heading", "crates/beta/README.md", Some(1)),
        ("documentation-header", "crates/gamma/src/extra.rs", Some(1)),
        ("documentation-module", "crates/delta/src/extra.rs", None),
        ("documentation-fence", "crates/epsilon/README.md", Some(3)),
        ("documentation-fence", "crates/epsilon/README.md", Some(7)),
        ("documentation-fence", "crates/epsilon/README.md", Some(11)),
        ("documentation-fence", "crates/epsilon/README.md", Some(15)),
        ("documentation-fence", "crates/epsilon/src/lib.rs", Some(6)),
        ("documentation-readme", "crates/zeta/README.md", None),
    ];
    for (rule, path, line) in expected {
        assert!(
            violations
                .iter()
                .any(|violation| is_at(violation, rule, path, *line)),
            "{rule} at {path}:{line:?} is reported: {violations:?}"
        );
    }
    assert_eq!(
        violations.len(),
        expected.len(),
        "nothing else is reported: {violations:?}"
    );
}

/// The clean sibling has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_documentation_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_documentation_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
