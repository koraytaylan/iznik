//! `xtask policy`: the policy checks clippy cannot express — whole-word
//! names from a committed vocabulary, no magic numbers, file length,
//! documentation placement, link integrity, the dependency allowlist, the
//! blocking-call boundary, the unsafe boundary, and the absence of `allow`,
//! `expect` and `cfg(test)` — each a pure function from a repository root to
//! a list of violations, proven against synthetic trees under
//! `xtask/tests/fixtures/policy/` that contain the violations it must catch.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use iznik_harness::process::ProcessError;
use syn::spanned::Spanned;

pub mod attributes;
pub mod blocking;
pub mod dependencies;
pub mod documentation;
pub mod length;
pub mod lexicon;
pub mod links;
pub mod literals;
pub mod unsafe_boundary;

/// The directories whose sources the source-level checks read, relative to
/// the root.
pub const SOURCE_ROOTS: &[&str] = &["crates", "xtask"];

/// The synthetic trees with deliberate violations, relative to the root. No
/// check reads them as part of the tree it is given: they are not the tree,
/// they are what the tree's checks are proven against.
pub const FIXTURES: &str = "xtask/tests/fixtures";

/// A check: a function from a repository root to the violations in it,
/// failing only when a file it must read cannot be read or parsed — a
/// tooling failure, which is not a violation and is not swallowed.
pub type Check = fn(&Path) -> Result<Vec<Violation>, PolicyError>;

/// The crate roots, relative to a crate's directory: a library's and a
/// binary's, each a crate root when present.
pub const CRATE_ROOTS: &[&str] = &["src/lib.rs", "src/main.rs"];

/// The directories at the root that hold no source: the build output and the
/// repository's own data.
const UNSCANNED_ROOT_DIRECTORIES: &[&str] = &["target", ".git"];

/// Every check with its name, in the order `xtask policy` runs them.
pub const CHECKS: &[(&str, Check)] = &[
    ("lexicon", lexicon::check),
    ("literals", literals::check),
    ("length", length::check),
    ("documentation", documentation::check),
    ("links", links::check),
    ("dependencies", dependencies::check),
    ("blocking", blocking::check),
    ("unsafe-boundary", unsafe_boundary::check),
    ("attributes", attributes::check),
];

/// One rule broken at one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The file, relative to the root.
    pub path: PathBuf,
    /// The line, when the rule is about a line.
    pub line: Option<usize>,
    /// The rule broken.
    pub rule: &'static str,
    /// What is wrong, in a sentence.
    pub detail: String,
}

impl Display for Violation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(
                formatter,
                "{}:{line}: {}: {}",
                self.path.display(),
                self.rule,
                self.detail
            ),
            None => write!(
                formatter,
                "{}: {}: {}",
                self.path.display(),
                self.rule,
                self.detail
            ),
        }
    }
}

/// Why a check could not run to completion — an unreadable file, a file that
/// does not parse, a tool that failed — as distinct from a violation it found.
#[derive(Debug)]
pub enum PolicyError {
    /// A file or directory could not be read.
    Io {
        /// The path.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// A file is not what its kind requires.
    Parse {
        /// The path.
        path: PathBuf,
        /// What the parser said.
        detail: String,
    },
    /// A tool the check runs did not complete.
    Process(ProcessError),
}

impl Display for PolicyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            PolicyError::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            PolicyError::Parse { path, detail } => {
                write!(formatter, "{}: {detail}", path.display())
            }
            PolicyError::Process(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PolicyError::Io { source, .. } => Some(source),
            PolicyError::Process(error) => Some(error),
            PolicyError::Parse { .. } => None,
        }
    }
}

/// Reads a file as text.
///
/// # Errors
///
/// [`PolicyError::Io`] when it cannot be read.
pub fn read(path: &Path) -> Result<String, PolicyError> {
    fs::read_to_string(path).map_err(|source| PolicyError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Parses a Rust source file.
///
/// # Errors
///
/// [`PolicyError::Io`] when it cannot be read and [`PolicyError::Parse`] when
/// it is not Rust.
pub fn parse_rust(path: &Path) -> Result<syn::File, PolicyError> {
    syn::parse_file(&read(path)?).map_err(|error| PolicyError::Parse {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

/// Parses a TOML file into a table.
///
/// # Errors
///
/// [`PolicyError::Io`] when it cannot be read and [`PolicyError::Parse`] when
/// it is not TOML.
pub fn parse_toml(path: &Path) -> Result<toml::Table, PolicyError> {
    toml::from_str(&read(path)?).map_err(|error| PolicyError::Parse {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

/// Whether `path` lies under the fixtures of `root`.
#[must_use]
pub fn is_fixture(root: &Path, path: &Path) -> bool {
    path.starts_with(root.join(FIXTURES))
}

/// The synthetic tree `tree` of the check `check` under `root`'s fixtures:
/// `xtask/tests/fixtures/policy/<check>/<tree>`.
#[must_use]
pub fn fixture_root(root: &Path, check: &str, tree: &str) -> PathBuf {
    root.join(FIXTURES).join("policy").join(check).join(tree)
}

/// `path` relative to `root`, for reporting; a path outside the root is
/// reported as it is.
#[must_use]
pub fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

/// Every file under `directory`, recursively, in sorted order, leaving out
/// the fixtures and the root's own `target` and `.git` directories. A
/// directory that does not exist has no files.
///
/// # Errors
///
/// [`PolicyError::Io`] when a directory cannot be listed.
pub fn files_under(root: &Path, directory: &Path) -> Result<Vec<PathBuf>, PolicyError> {
    let mut files = Vec::new();
    if !directory.is_dir() {
        return Ok(files);
    }
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = fs::read_dir(&current).map_err(|source| PolicyError::Io {
            path: current.clone(),
            source,
        })?;
        for entry in entries {
            let path = entry
                .map_err(|source| PolicyError::Io {
                    path: current.clone(),
                    source,
                })?
                .path();
            let unscanned = path.parent() == Some(root)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| UNSCANNED_ROOT_DIRECTORIES.contains(&name));
            if is_fixture(root, &path) || unscanned {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Every `.rs` file under the source roots of `root`, sorted.
///
/// # Errors
///
/// [`PolicyError::Io`] when a directory cannot be listed.
pub fn rust_files(root: &Path) -> Result<Vec<PathBuf>, PolicyError> {
    let mut files = Vec::new();
    for source_root in SOURCE_ROOTS {
        files.extend(
            files_under(root, &root.join(source_root))?
                .into_iter()
                .filter(|path| path.extension().is_some_and(|extension| extension == "rs")),
        );
    }
    Ok(files)
}

/// The directories of the workspace members named by the root manifest, in
/// manifest order.
///
/// # Errors
///
/// [`PolicyError::Io`] and [`PolicyError::Parse`] for the root manifest.
pub fn workspace_members(root: &Path) -> Result<Vec<PathBuf>, PolicyError> {
    let manifest = parse_toml(&root.join("Cargo.toml"))?;
    Ok(manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(|member| root.join(member))
        .collect())
}

/// The `package.name` a parsed manifest declares, if any.
#[must_use]
pub fn package_name_of(manifest: &toml::Table) -> Option<&str> {
    manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
}

/// The `package.name` of a crate manifest.
///
/// # Errors
///
/// [`PolicyError::Io`] and [`PolicyError::Parse`] for the manifest, and
/// [`PolicyError::Parse`] when it names no package.
pub fn package_name(manifest_path: &Path) -> Result<String, PolicyError> {
    package_name_of(&parse_toml(manifest_path)?)
        .map(str::to_owned)
        .ok_or_else(|| PolicyError::Parse {
            path: manifest_path.to_path_buf(),
            detail: "no package.name".to_owned(),
        })
}

/// Where a line of Markdown stands relative to fenced code blocks.
#[derive(Debug)]
pub enum FenceLine<'text> {
    /// The line opens a fence; `tag` is what follows the backticks.
    Opening {
        /// The line, one-based.
        line: usize,
        /// The info string after the backticks, trimmed.
        tag: &'text str,
    },
    /// The line closes a fence.
    Closing {
        /// The line, one-based.
        line: usize,
    },
    /// The line is inside a fence.
    Inside {
        /// The line, one-based.
        line: usize,
    },
    /// The line is prose, outside any fence.
    Outside {
        /// The line, one-based.
        line: usize,
        /// The line's text.
        text: &'text str,
    },
}

/// Walks lines of Markdown — `(one-based line, text)` — telling fences from
/// prose, the one place the fence rule is written.
pub fn fence_lines<'text>(
    lines: impl Iterator<Item = (usize, &'text str)>,
) -> Vec<FenceLine<'text>> {
    let mut inside = false;
    lines
        .map(|(line, text)| match text.trim_start().strip_prefix("```") {
            Some(rest) if !inside => {
                inside = true;
                FenceLine::Opening {
                    line,
                    tag: rest.trim(),
                }
            }
            Some(_closing) => {
                inside = false;
                FenceLine::Closing { line }
            }
            None if inside => FenceLine::Inside { line },
            None => FenceLine::Outside { line, text },
        })
        .collect()
}

/// The line a syntax node starts on, one-based.
#[must_use]
pub fn line_of(node: &impl Spanned) -> usize {
    node.span().start().line
}

/// The entry point of `xtask policy`: every check in order, every violation
/// as `path:line: rule: detail` on standard output, and a failure status when
/// there is one.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.len() != 1 {
        writeln!(io::stderr(), "usage: xtask policy").unwrap_or_default();
        return ExitCode::from(crate::USAGE_EXIT_CODE);
    }
    let root = crate::repository_root();
    let mut clean = true;
    for (name, check) in CHECKS {
        match check(&root) {
            Ok(violations) => {
                for violation in violations {
                    clean = false;
                    writeln!(io::stdout(), "{violation}").unwrap_or_default();
                }
            }
            Err(error) => {
                writeln!(io::stderr(), "policy: {name}: {error}").unwrap_or_default();
                return ExitCode::FAILURE;
            }
        }
    }
    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
