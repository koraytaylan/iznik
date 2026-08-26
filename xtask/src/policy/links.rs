//! Every relative link and reference target in every Markdown file resolves
//! against the tree. A target with a scheme or a bare fragment is not this
//! check's to resolve; a fragment on a path is dropped before the path is; a
//! link inside a fence or inline code is shown, not made; a root-absolute
//! target resolves against the repository, not the filesystem.

use std::path::Path;

use super::{FenceLine, PolicyError, Violation, fence_lines, files_under, read, relative};

/// The rule a dangling link breaks.
const RULE: &str = "link";

/// What separates a link's text from its target.
const LINK_OPENING: &str = "](";

/// What begins a footnote's label, which defines no link target.
const FOOTNOTE_LABEL_PREFIX: char = '^';

/// Whether a link target is a URL rather than a path.
fn has_scheme(target: &str) -> bool {
    let Some((scheme, _rest)) = target.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
        })
}

/// A line with its inline code spans removed: a link inside backticks is
/// shown, not made.
fn without_inline_code(line: &str) -> String {
    let mut inside_code = false;
    let mut kept = String::new();
    for piece in line.split('`') {
        if !inside_code {
            kept.push_str(piece);
        }
        inside_code = !inside_code;
    }
    kept
}

/// The link targets on one line: every `](target)` and, on a line of the
/// form `[label]: target` that is not a footnote, that target; inline code
/// left out.
fn targets_on(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let without_code = without_inline_code(line);
    let mut rest = without_code.as_str();
    while let Some(start) = rest.find(LINK_OPENING) {
        let after = rest
            .get(start.saturating_add(LINK_OPENING.len())..)
            .unwrap_or_default();
        let Some(end) = after.find(')') else {
            break;
        };
        let inside = after.get(..end).unwrap_or_default();
        let target = inside.split_whitespace().next().unwrap_or_default();
        targets.push(target.to_owned());
        rest = after.get(end..).unwrap_or_default();
    }
    let trimmed = without_code.trim_start();
    if let Some(definition) = trimmed.strip_prefix('[')
        && !definition.starts_with(FOOTNOTE_LABEL_PREFIX)
        && let Some((label, target)) = definition.split_once("]:")
        && !label.contains(']')
        && let Some(target) = target.split_whitespace().next()
    {
        targets.push(target.to_owned());
    }
    targets
}

/// The path part of a target, with a fragment or query removed; `None` for a
/// target this check does not resolve.
fn path_of(target: &str) -> Option<&str> {
    if target.is_empty() || target.starts_with('#') || has_scheme(target) {
        return None;
    }
    let without_fragment = target.split(['#', '?']).next().unwrap_or_default();
    if without_fragment.is_empty() {
        return None;
    }
    Some(without_fragment.trim_matches('<').trim_matches('>'))
}

/// Where a target resolves from: the repository root for a root-absolute
/// target, the file's own directory otherwise.
fn base_of<'directories, 'target>(
    root: &'directories Path,
    directory: &'directories Path,
    target: &'target str,
) -> (&'directories Path, &'target str) {
    match target.strip_prefix('/') {
        Some(from_root) => (root, from_root),
        None => (directory, target),
    }
}

/// The links check.
///
/// # Errors
///
/// [`PolicyError::Io`] when a directory cannot be listed or a file read.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    for path in files_under(root, root)? {
        if path.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let document = read(&path)?;
        let directory = path.parent().unwrap_or(root);
        let lines = document
            .lines()
            .enumerate()
            .map(|(index, line)| (index.saturating_add(1), line));
        for fence in fence_lines(lines) {
            let FenceLine::Outside { line, text } = fence else {
                continue;
            };
            for target in targets_on(text) {
                let Some(relative_target) = path_of(&target) else {
                    continue;
                };
                let (base, from_base) = base_of(root, directory, relative_target);
                if !base.join(from_base).exists() {
                    violations.push(Violation {
                        path: relative(root, &path),
                        line: Some(line),
                        rule: RULE,
                        detail: format!("`{target}` does not resolve"),
                    });
                }
            }
        }
    }
    Ok(violations)
}
