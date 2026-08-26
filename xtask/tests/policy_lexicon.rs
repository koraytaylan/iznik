//! The vocabulary check against its synthetic trees and the real tree: a
//! declared identifier with a word outside the vocabulary is reported with
//! its file and line, a used-but-undeclared identifier is not, every form of
//! declaration is split as the rule says, a file name with an unlisted word
//! is reported, and a vocabulary file that is unsorted, has a duplicate, has
//! a malformed entry or is named after no task is reported.

use std::path::{Path, PathBuf};

use xtask::policy::Violation;
use xtask::policy::fixture_root;
use xtask::policy::lexicon::{check, words};
use xtask::repository_root;

/// The check's fixture tree `tree`.
fn fixture(tree: &str) -> PathBuf {
    fixture_root(&repository_root(), "lexicon", tree)
}

/// The violations of one rule, in order.
fn of_rule<'violations>(
    violations: &'violations [Violation],
    rule: &str,
) -> Vec<&'violations Violation> {
    violations
        .iter()
        .filter(|violation| violation.rule == rule)
        .collect()
}

/// Whether a violation is at a path and line with a detail containing text.
fn is_at(violation: &Violation, path: &str, line: Option<usize>, text: &str) -> bool {
    violation.path == Path::new(path) && violation.line == line && violation.detail.contains(text)
}

/// Identifiers split into words as the rule says: `snake_case`, `CamelCase`,
/// `SCREAMING_CASE`, trailing digits, digit-only tokens, raw identifiers,
/// leading underscores, hyphens and file extensions.
///
/// # Panics
///
/// When a split differs.
#[test]
fn policy_lexicon_splits_identifiers_into_words() {
    let cases: &[(&str, &[&str])] = &[
        ("snake_case_name", &["snake", "case", "name"]),
        ("CamelCaseName", &["camel", "case", "name"]),
        ("SCREAMING_CASE", &["screaming", "case"]),
        ("HTTPServer", &["http", "server"]),
        ("utf8_value", &["utf8", "value"]),
        ("sha256", &["sha256"]),
        ("x86_64", &["x86"]),
        ("r#type", &["type"]),
        ("_unused", &["unused"]),
        ("__double", &["double"]),
        ("iznik-testkit", &["iznik", "testkit"]),
        (
            "regression_scenarios.rs",
            &["regression", "scenarios", "rs"],
        ),
        ("Cargo.toml", &["cargo", "toml"]),
        ("PtyChild", &["pty", "child"]),
    ];
    for (identifier, expected) in cases {
        assert_eq!(&words(identifier), expected, "the words of {identifier}");
    }
}

/// A declared identifier with a word outside the vocabulary is reported with
/// its file and line; a used-but-undeclared identifier, such as a method
/// from `std`, is not; and every other form of declaration in the fixture is
/// split into words the vocabulary holds.
///
/// # Panics
///
/// When the report differs.
#[test]
fn policy_lexicon_reports_a_declared_word_outside_the_vocabulary() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let identifiers = of_rule(&violations, "lexicon");
    assert!(
        identifiers.iter().any(|violation| is_at(
            violation,
            "crates/example/src/lib.rs",
            Some(5),
            "`read_buf`: `buf`"
        )),
        "the function name is reported: {identifiers:?}"
    );
    assert!(
        identifiers.iter().any(|violation| is_at(
            violation,
            "crates/example/src/lib.rs",
            Some(5),
            "`buf`: `buf`"
        )),
        "the parameter is reported: {identifiers:?}"
    );
    assert!(
        identifiers
            .iter()
            .all(|violation| violation.detail.contains("`buf`")),
        "nothing but `buf` is reported: {identifiers:?}"
    );
}

/// A file name with an unlisted word is reported.
///
/// # Panics
///
/// When the file is not reported, or another is.
#[test]
fn policy_lexicon_reports_a_file_name_outside_the_vocabulary() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let names = of_rule(&violations, "lexicon-name");
    assert_eq!(names.len(), 1, "one name is reported: {names:?}");
    assert!(
        is_at(
            names.first().expect("one"),
            "crates/example/src/buf.rs",
            None,
            "`buf`"
        ),
        "the abbreviation is reported: {names:?}"
    );
}

/// A vocabulary file that is unsorted, has a duplicate, has an uppercase or
/// non-alphanumeric entry, or is named after no task is reported.
///
/// # Panics
///
/// When any of those is not reported where it is.
#[test]
fn policy_lexicon_reports_malformed_vocabulary_files() {
    let violations = check(&fixture("violating")).expect("the check runs");
    let files = of_rule(&violations, "lexicon-file");
    let expected: &[(&str, Option<usize>, &str)] = &[
        ("policy/lexicon/unsorted-task.txt", Some(4), "out of order"),
        ("policy/lexicon/duplicate-task.txt", Some(2), "listed twice"),
        ("policy/lexicon/malformed-task.txt", Some(1), "`9nine`"),
        ("policy/lexicon/malformed-task.txt", Some(2), "`Capital`"),
        (
            "policy/lexicon/malformed-task.txt",
            Some(3),
            "`hyphen-ated`",
        ),
        ("policy/lexicon/orphan.txt", None, "not the id of a task"),
    ];
    for (path, line, text) in expected {
        assert!(
            files
                .iter()
                .any(|violation| is_at(violation, path, *line, text)),
            "{path}:{line:?} {text} is reported: {files:?}"
        );
    }
    assert_eq!(
        files.len(),
        expected.len(),
        "nothing else is reported: {files:?}"
    );
}

/// The clean sibling has no violations.
///
/// # Panics
///
/// When it has.
#[test]
fn policy_lexicon_clean_tree_has_no_violations() {
    let violations = check(&fixture("clean")).expect("the check runs");
    assert!(violations.is_empty(), "{violations:?}");
}

/// The real tree passes.
///
/// # Panics
///
/// When it does not, listing every violation.
#[test]
fn policy_lexicon_real_tree_is_clean() {
    let violations = check(&repository_root()).expect("the check runs");
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(violations.is_empty(), "{}", report.join("\n"));
}
