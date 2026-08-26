//! Every crate root includes its README, every README exists and begins with
//! the crate's heading, every source file begins with documentation, every
//! module of a crate appears in its README, and every fenced block in a
//! README or a doc comment carries a language tag that is not a doctest's —
//! an untagged fence in documentation is a doctest that nextest never runs,
//! which is documentation that can rot without failing anything.

use std::path::Path;

use super::{
    CRATE_ROOTS, FenceLine, PolicyError, Violation, fence_lines, files_under, package_name, read,
    relative, rust_files, workspace_members,
};

/// The line every crate root carries.
const README_INCLUDE: &str = "#![doc = include_str!(\"../README.md\")]";

/// The language tag under which rustdoc compiles a fence as a test.
const RUST_TAG: &str = "rust";

/// The fence attributes rustdoc understands without a language, each of
/// which makes the fence a doctest: a fence whose first word is one of these
/// is `rust` in disguise.
const DOCTEST_ATTRIBUTES: &[&str] = &[
    "ignore",
    "no_run",
    "should_panic",
    "compile_fail",
    "test_harness",
    "standalone_crate",
];

/// The prefix of the fence attributes naming an edition, which likewise make
/// a fence a doctest.
const EDITION_ATTRIBUTE_PREFIX: &str = "edition";

/// The rule a crate root without the include breaks.
const INCLUDE_RULE: &str = "documentation-include";

/// The rule a missing README breaks.
const README_RULE: &str = "documentation-readme";

/// The rule a README without the crate heading breaks.
const HEADING_RULE: &str = "documentation-heading";

/// The rule a source file that does not begin with documentation breaks.
const HEADER_RULE: &str = "documentation-header";

/// The rule a module absent from its crate's README breaks.
const MODULE_RULE: &str = "documentation-module";

/// The rule a doctest fence breaks.
const FENCE_RULE: &str = "documentation-fence";

/// The module path of a file under `src/`, as `pty::spawn` — a `mod.rs` is
/// its directory's module — or `None` for a crate root.
fn module_path(source: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(source).ok()?;
    let mut segments: Vec<String> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(|segment| segment.trim_end_matches(".rs").to_owned())
        .collect();
    if segments.last().is_some_and(|last| last == "mod") {
        segments.pop();
    }
    if segments.is_empty()
        || matches!(segments.as_slice(), [root] if root == "lib" || root == "main")
    {
        return None;
    }
    Some(segments.join("::"))
}

/// Why a fence's tag makes it a doctest, or `None` when it does not: an
/// empty tag, `rust`, or a bare rustdoc attribute, judged on the first
/// comma-separated word.
fn doctest_reason(tag: &str) -> Option<String> {
    let first = tag.split(',').next().unwrap_or_default().trim();
    if first.is_empty() {
        return Some("an untagged fence is a doctest that never runs; tag it".to_owned());
    }
    if first == RUST_TAG
        || DOCTEST_ATTRIBUTES.contains(&first)
        || first.starts_with(EDITION_ATTRIBUTE_PREFIX)
    {
        return Some(format!(
            "a fence tagged `{tag}` is a doctest that never runs; tag it `text`"
        ));
    }
    None
}

/// Reports the doctest fences of one documentation text: `lines` are the
/// lines of a README, or the bodies of a run of doc comments.
fn check_fences<'text>(
    lines: impl Iterator<Item = (usize, &'text str)>,
    path: &Path,
    violations: &mut Vec<Violation>,
) {
    for fence in fence_lines(lines) {
        let FenceLine::Opening { line, tag } = fence else {
            continue;
        };
        if let Some(detail) = doctest_reason(tag) {
            violations.push(Violation {
                path: path.to_path_buf(),
                line: Some(line),
                rule: FENCE_RULE,
                detail,
            });
        }
    }
}

/// The doc-comment lines of a source file — `///` and `//!` — with their
/// bodies and one-based line numbers.
fn documentation_comment_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim_start();
            let body = trimmed
                .strip_prefix("///")
                .or_else(|| trimmed.strip_prefix("//!"))?;
            Some((index.saturating_add(1), body))
        })
        .collect()
}

/// One crate: its roots include the README, the README exists and begins
/// with the heading, and every module under `src/` appears in the README.
///
/// # Errors
///
/// [`PolicyError`] when the manifest or a root cannot be read.
fn check_crate(
    root: &Path,
    crate_directory: &Path,
    violations: &mut Vec<Violation>,
) -> Result<(), PolicyError> {
    let name = package_name(&crate_directory.join("Cargo.toml"))?;
    for crate_root in CRATE_ROOTS {
        let path = crate_directory.join(crate_root);
        if !path.exists() {
            continue;
        }
        if !read(&path)?.contains(README_INCLUDE) {
            violations.push(Violation {
                path: relative(root, &path),
                line: None,
                rule: INCLUDE_RULE,
                detail: format!("the crate root does not carry `{README_INCLUDE}`"),
            });
        }
    }
    let readme_path = crate_directory.join("README.md");
    if !readme_path.exists() {
        violations.push(Violation {
            path: relative(root, &readme_path),
            line: None,
            rule: README_RULE,
            detail: "the crate has no README, which is its documentation".to_owned(),
        });
        return Ok(());
    }
    let readme = read(&readme_path)?;
    let heading = format!("# {name}");
    if readme.lines().next() != Some(heading.as_str()) {
        violations.push(Violation {
            path: relative(root, &readme_path),
            line: Some(1),
            rule: HEADING_RULE,
            detail: format!("the README does not begin with `{heading}`"),
        });
    }
    check_fences(
        readme
            .lines()
            .enumerate()
            .map(|(index, line)| (index.saturating_add(1), line)),
        &relative(root, &readme_path),
        violations,
    );
    let source = crate_directory.join("src");
    for file in files_under(root, &source)? {
        if file.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let Some(module) = module_path(&source, &file) else {
            continue;
        };
        if !readme.contains(&format!("`{module}`")) {
            violations.push(Violation {
                path: relative(root, &file),
                line: None,
                rule: MODULE_RULE,
                detail: format!(
                    "module `{module}` does not appear in {}",
                    relative(root, &readme_path).display()
                ),
            });
        }
    }
    Ok(())
}

/// One source file: it begins with a `//!` line, and its doc comments' fences
/// are tagged.
///
/// # Errors
///
/// [`PolicyError::Io`] when the file cannot be read.
fn check_source(
    root: &Path,
    path: &Path,
    violations: &mut Vec<Violation>,
) -> Result<(), PolicyError> {
    let text = read(path)?;
    let reported = relative(root, path);
    if !text.starts_with("//!") {
        violations.push(Violation {
            path: reported.clone(),
            line: Some(1),
            rule: HEADER_RULE,
            detail: "the file does not begin with a `//!` documentation line".to_owned(),
        });
    }
    check_fences(
        documentation_comment_lines(&text).into_iter(),
        &reported,
        violations,
    );
    Ok(())
}

/// The documentation check.
///
/// # Errors
///
/// [`PolicyError`] when a manifest, crate root or source file cannot be
/// read.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    for crate_directory in workspace_members(root)? {
        check_crate(root, &crate_directory, &mut violations)?;
    }
    for path in rust_files(root)? {
        check_source(root, &path, &mut violations)?;
    }
    Ok(violations)
}
