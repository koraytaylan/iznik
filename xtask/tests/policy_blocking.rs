//! The blocking check against its synthetic trees and the real tree:
//! `std::thread::sleep`, `std::io::Read`, `std::io::stdout` and
//! `std::process::Command` under `iznik-server` or `iznik-client` are
//! reported, through an import, a glob or a macro invocation too; the same in
//! `crates/iznik-server/src/pty/streams.rs` or in any other crate are not;
//! `std::process`'s inert types are not.

use std::path::{Path, PathBuf};

use xtask::policy::Violation;
use xtask::policy::blocking::check;
use xtask::policy::fixture_root;
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "blocking", tree)
}

/// Whether a violation is at a path and line with a detail containing text.
fn is_at(violation: &Violation, path: &str, line: Option<usize>, text: &str) -> bool {
    violation.path == Path::new(path) && violation.line == line && violation.detail.contains(text)
}

/// Every blocking facility in the two asynchronous crates is reported with
/// its line, whether named in full, through an import or through a glob;
/// the inert `std::process` names, the named exception and other crates are
/// not.
///
/// # Panics
///
/// When the report differs.
#[test]
fn policy_blocking_reports_facilities_in_the_asynchronous_crates_only() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let server = "crates/iznik-server/src/lib.rs";
    let client = "crates/iznik-client/src/lib.rs";
    let expected: &[(&str, Option<usize>, &str)] = &[
        (server, Some(2), "`std::io::Read`"),
        (server, Some(6), "`std::thread::sleep`"),
        (server, Some(7), "`std::io::stdout`"),
        (server, Some(8), "`std::process::Command`"),
        (server, Some(12), "`std::io::stderr`"),
        (server, Some(13), "`std::io::stdout`"),
        (client, Some(2), "`std::io::Write`"),
        (client, Some(3), "glob import of `std::io`"),
        (client, Some(4), "glob import of `std::io::prelude`"),
    ];
    for (path, line, text) in expected {
        assert!(
            violations
                .iter()
                .any(|violation| is_at(violation, path, *line, text)),
            "{path}:{line:?} {text} is reported: {violations:?}"
        );
    }
    assert_eq!(
        violations.len(),
        expected.len(),
        "nothing else is reported: {violations:?}"
    );
}

/// The clean sibling — inert `std::process` names and the runtime's streams
/// — has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_blocking_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_blocking_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
