//! `THIRD-PARTY-NOTICES`: the licences of every package a shipped binary is
//! built from.
//!
//! What a binary contains is the normal dependency graph of its package for
//! its target: `cargo metadata --filter-platform` resolves that from
//! `Cargo.lock` without the network, once the sources are in cargo's registry
//! — which they are, because the build that made the binary put them there.
//! Build and development dependencies are left out, since none of their code
//! is linked; the workspace's own packages are left out, since they are this
//! repository's and under its licence.
//!
//! Each package is listed with its version, its licence expression and its
//! repository, and every licence text the packages carry — the `LICENSE*`,
//! `LICENCE*`, `COPYING*` and `NOTICE*` files at the root of each, or the
//! file the manifest names — is printed once, followed by the packages that
//! carry it. Code a `-sys` package builds from another language is covered by
//! that package's own licence files.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};
use serde_json::Value;

use crate::distribution::DistributionError;

/// The file this writes beside a binary and into a bundle.
pub const NOTICES: &str = "THIRD-PARTY-NOTICES";

/// How long `cargo metadata` may take: it reads the lock file and the
/// registry, and a first run may resolve the workspace.
const METADATA_DEADLINE: Duration = Duration::from_mins(2);

/// The prefixes, compared without case, of the files that hold a licence
/// text at a package's root.
const LICENCE_PREFIXES: &[&str] = &["license", "licence", "copying", "notice"];

/// What a package that declares no licence is listed with.
pub const UNDECLARED: &str = "not declared";

/// The rule between the sections of the file.
const RULE: &str =
    "--------------------------------------------------------------------------------";

/// One package the binary is built from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Its licence expression, or [`UNDECLARED`].
    pub licence: String,
    /// Where its source lives, when it says.
    pub repository: Option<String>,
    /// The licence texts it carries, each as written.
    pub texts: Vec<String>,
}

/// Every package `root_package` is built from for `target`, sorted by name
/// and version, with the workspace's own packages left out.
///
/// # Errors
///
/// [`DistributionError::Build`] when `cargo metadata` fails or answers with
/// something this cannot read, naming the target; [`DistributionError::Io`]
/// when a licence file cannot be read.
pub fn packages(
    root: &Path,
    root_package: &str,
    target: &str,
) -> Result<Vec<Package>, DistributionError> {
    let metadata = metadata(root, target)?;
    let unreadable = |detail: &str| DistributionError::Build {
        target: target.to_owned(),
        detail: format!("cargo metadata: {detail}"),
    };
    let all = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| unreadable("no packages"))?;
    let start = all
        .iter()
        .find(|package| {
            field(package, "name") == Some(root_package)
                && package.get("source").is_some_and(Value::is_null)
        })
        .and_then(|package| field(package, "id"))
        .ok_or_else(|| unreadable(&format!("no workspace package named {root_package}")))?;
    let reached = linked_from(&metadata, start);
    let mut found = Vec::new();
    for package in all {
        let external = !package.get("source").is_some_and(Value::is_null);
        if external && field(package, "id").is_some_and(|id| reached.contains(id)) {
            found.push(described(package)?);
        }
    }
    found.sort();
    Ok(found)
}

/// Every package id reachable from `start` through normal dependencies, in
/// the resolve graph of `metadata`, `start` included.
fn linked_from<'metadata>(
    metadata: &'metadata Value,
    start: &'metadata str,
) -> BTreeSet<&'metadata str> {
    let nodes: BTreeMap<&str, &Vec<Value>> = metadata
        .get("resolve")
        .and_then(|resolve| resolve.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| Some((field(node, "id")?, node.get("deps")?.as_array()?)))
                .collect()
        })
        .unwrap_or_default();
    let mut reached = BTreeSet::from([start]);
    let mut waiting = vec![start];
    while let Some(id) = waiting.pop() {
        for dependency in nodes.get(id).copied().into_iter().flatten() {
            let Some(package) = field(dependency, "pkg") else {
                continue;
            };
            if is_linked(dependency) && reached.insert(package) {
                waiting.push(package);
            }
        }
    }
    reached
}

/// Whether a dependency edge is a normal one — linked into what depends on
/// it — rather than only a build or development dependency.
fn is_linked(dependency: &Value) -> bool {
    dependency
        .get("dep_kinds")
        .and_then(Value::as_array)
        .is_some_and(|kinds| {
            kinds
                .iter()
                .any(|kind| kind.get("kind").is_some_and(Value::is_null))
        })
}

/// A package from its metadata, with the licence texts beside its manifest.
///
/// # Errors
///
/// [`DistributionError::Io`] when a licence file cannot be read.
fn described(package: &Value) -> Result<Package, DistributionError> {
    let directory = field(package, "manifest_path")
        .map(Path::new)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut files = licence_files(&directory);
    if let Some(named) = field(package, "license_file") {
        files.insert(directory.join(named));
    }
    let mut texts = Vec::new();
    for file in files {
        let bytes = std::fs::read(&file).map_err(|source| DistributionError::Io {
            path: file.clone(),
            source,
        })?;
        texts.push(String::from_utf8_lossy(&bytes).trim().to_owned());
    }
    Ok(Package {
        name: field(package, "name").unwrap_or_default().to_owned(),
        version: field(package, "version").unwrap_or_default().to_owned(),
        licence: field(package, "license").unwrap_or(UNDECLARED).to_owned(),
        repository: field(package, "repository").map(str::to_owned),
        texts,
    })
}

/// The licence files at the root of a package's directory, sorted.
fn licence_files(directory: &Path) -> BTreeSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let lowered = name.to_string_lossy().to_lowercase();
                LICENCE_PREFIXES
                    .iter()
                    .any(|prefix| lowered.starts_with(prefix))
            })
        })
        .collect()
}

/// A string field of a JSON object.
fn field<'value>(value: &'value Value, name: &str) -> Option<&'value str> {
    value.get(name).and_then(Value::as_str)
}

/// The resolved metadata for `target`, read offline from `Cargo.lock` and
/// the registry.
///
/// # Errors
///
/// [`DistributionError::Build`] when cargo fails or does not answer JSON.
fn metadata(root: &Path, target: &str) -> Result<Value, DistributionError> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root).args([
        "metadata",
        "--format-version",
        "1",
        "--locked",
        "--offline",
        "--filter-platform",
        target,
    ]);
    let failed = |detail: String| DistributionError::Build {
        target: target.to_owned(),
        detail: format!("cargo metadata: {detail}"),
    };
    let completed = process::run(command, Deadline(METADATA_DEADLINE), Output::Whole)
        .map_err(|error| failed(error.to_string()))?;
    serde_json::from_slice(&completed.stdout).map_err(|error| failed(error.to_string()))
}

/// The notices for a set of packages: a heading saying what the file is, one
/// line per package, then each distinct licence text once with the packages
/// that carry it.
#[must_use]
pub fn render(what: &str, packages: &[Package]) -> String {
    let mut said = format!(
        "Third-party notices for {what}\n\n\
         {what} is built from the packages below, each under the licence it\n\
         declares. Every licence text they carry follows once, with the\n\
         packages that carry it.\n\n{RULE}\nPackages\n{RULE}\n\n"
    );
    for package in packages {
        let repository = package
            .repository
            .as_deref()
            .unwrap_or("no repository given");
        // Writing to a `String` does not fail.
        let _written = writeln!(
            said,
            "{} {} - {} - {repository}",
            package.name, package.version, package.licence
        );
    }
    let mut carriers: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for package in packages {
        for text in &package.texts {
            carriers
                .entry(text.as_str())
                .or_default()
                .insert(format!("{} {}", package.name, package.version));
        }
    }
    let without: Vec<String> = packages
        .iter()
        .filter(|package| package.texts.is_empty())
        .map(|package| format!("{} {}", package.name, package.version))
        .collect();
    if !without.is_empty() {
        let _written = write!(
            said,
            "\nThese packages ship no licence file; their licence is the one they declare\nabove, whose standard text is among those below: {}\n",
            without.join(", ")
        );
    }
    for (text, holders) in carriers {
        let named: Vec<String> = holders.into_iter().collect();
        let _written = write!(
            said,
            "\n{RULE}\nCarried by: {}\n{RULE}\n\n{text}\n",
            named.join(", ")
        );
    }
    said
}

/// Writes the notices for the packages of every `(package, target)` pair,
/// merged, to `path`.
///
/// # Errors
///
/// As [`packages`], and [`DistributionError::Io`] when the file cannot be
/// written.
pub fn write(
    root: &Path,
    what: &str,
    roots: &[(&str, &str)],
    path: &Path,
) -> Result<(), DistributionError> {
    let mut merged = BTreeSet::new();
    for (root_package, target) in roots {
        merged.extend(packages(root, root_package, target)?);
    }
    let listed: Vec<Package> = merged.into_iter().collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DistributionError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, render(what, &listed)).map_err(|source| DistributionError::Io {
        path: path.to_path_buf(),
        source,
    })
}
