//! The dependencies check against its synthetic workspaces and the real
//! tree: a resolved package absent from the allowlist and a listed package
//! nothing resolves are both reported; a version range, in a development
//! dependency too, is reported; a `build.rs` in a workspace crate is
//! reported; a `[dependencies]` entry on `iznik-protocol` is reported and a
//! `[dev-dependencies]` entry is not.

use std::path::{Path, PathBuf};

use xtask::policy::Violation;
use xtask::policy::dependencies::check;
use xtask::policy::fixture_root;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "dependencies", tree)
}

/// Whether a violation is of a rule at a path with a detail containing text.
fn is_at(violation: &Violation, rule: &str, path: &str, text: &str) -> bool {
    violation.rule == rule && violation.path == Path::new(path) && violation.detail.contains(text)
}

/// Each rule is reported where the fixture breaks it, and nowhere else.
///
/// # Panics
///
/// When a rule is not reported where it is broken, or the count differs.
#[test]
fn policy_dependencies_reports_each_rule_where_it_is_broken() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let expected: &[(&str, &str, &str)] = &[
        ("dependency-undeclared", "policy/dependencies.md", "`thing`"),
        ("dependency-unused", "policy/dependencies.md", "`phantom`"),
        (
            "dependency-pin",
            "crates/iznik-protocol/Cargo.toml",
            "dev-dependencies.thing",
        ),
        (
            "dependency-free-crate",
            "crates/iznik-protocol/Cargo.toml",
            "`iznik-protocol` declares",
        ),
        ("build-script", "crates/helper/build.rs", "build script"),
    ];
    for (rule, path, text) in expected {
        assert!(
            violations
                .iter()
                .any(|violation| is_at(violation, rule, path, text)),
            "{rule} at {path} ({text}) is reported: {violations:?}"
        );
    }
    assert_eq!(
        violations.len(),
        expected.len(),
        "nothing else is reported: {violations:?}"
    );
}

/// The clean sibling — a pinned path dependency in both tables, listed once
/// — has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_dependencies_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes: the allowlist equals the resolved set, every
/// requirement is a pin, the protocol crate has no dependencies, and no crate
/// has a build script.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_dependencies_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
