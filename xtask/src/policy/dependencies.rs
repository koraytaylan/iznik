//! The resolved package set equals the allowlist in `policy/dependencies.md`
//! in both directions, every version requirement in every workspace manifest
//! is an exact pin, `iznik-protocol` declares no dependencies, and no crate
//! has a build script. A dependency nobody declared fails the build; so does
//! a listed one nothing uses.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};

use super::{
    PolicyError, Violation, package_name_of, parse_toml, read, relative, workspace_members,
};

/// The allowlist, relative to the root.
const ALLOWLIST: &str = "policy/dependencies.md";

/// The heading under which the allowlist's entries are read.
const ALLOWLIST_HEADING: &str = "## Workspace";

/// The crate that declares no `[dependencies]`.
const DEPENDENCY_FREE_CRATE: &str = "iznik-protocol";

/// The dependency tables a manifest may carry.
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

/// How long `cargo metadata` may take: two minutes, which covers a first run
/// that reads every manifest in the tree and a hung cargo.
const METADATA_DEADLINE: Duration = Duration::from_mins(2);

/// The rule a resolved package absent from the allowlist breaks.
const UNDECLARED_RULE: &str = "dependency-undeclared";

/// The rule an allowlisted package nothing resolves breaks.
const UNUSED_RULE: &str = "dependency-unused";

/// The rule a version requirement that is not an exact pin breaks.
const PIN_RULE: &str = "dependency-pin";

/// The rule a `[dependencies]` table on the dependency-free crate breaks.
const DEPENDENCY_FREE_RULE: &str = "dependency-free-crate";

/// The rule a build script breaks.
const BUILD_SCRIPT_RULE: &str = "build-script";

/// Where cargo keeps its state for a metadata run: the target directory the
/// environment names, else the repository's own — never one under the root
/// being checked, which may be a synthetic tree in which a `target/` would be
/// an untracked file.
fn target_directory() -> std::ffi::OsString {
    std::env::var_os("CARGO_TARGET_DIR")
        .unwrap_or_else(|| crate::repository_root().join("target").into_os_string())
}

/// The names of the packages `cargo metadata` resolves for the workspace,
/// its own members left out.
///
/// # Errors
///
/// [`PolicyError::Process`] when cargo fails and [`PolicyError::Parse`] when
/// its output is not the metadata.
fn resolved_packages(root: &Path) -> Result<BTreeSet<String>, PolicyError> {
    let mut command = Command::new("cargo");
    command
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", target_directory());
    // The resolved tree's metadata is a multi-megabyte JSON document, so it is
    // read whole: a captured tail is not JSON, and the check's whole job is to
    // read this document.
    let completed = process::run(command, Deadline(METADATA_DEADLINE), Output::Whole)
        .map_err(PolicyError::Process)?;
    let metadata: serde_json::Value =
        serde_json::from_slice(&completed.stdout).map_err(|error| PolicyError::Parse {
            path: root.join("Cargo.toml"),
            detail: format!("cargo metadata: {error}"),
        })?;
    let members: BTreeSet<&str> = metadata
        .get("workspace_members")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    Ok(metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|package| {
            package
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| !members.contains(id))
        })
        .filter_map(|package| package.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect())
}

/// The backticked names under the allowlist's heading.
///
/// # Errors
///
/// [`PolicyError::Io`] when the allowlist cannot be read.
fn allowlisted_packages(root: &Path) -> Result<BTreeSet<String>, PolicyError> {
    let text = read(&root.join(ALLOWLIST))?;
    let mut names = BTreeSet::new();
    let mut under_heading = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            under_heading = line.trim() == ALLOWLIST_HEADING;
            continue;
        }
        if !under_heading {
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("- `")
            && let Some((name, _justification)) = rest.split_once('`')
        {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

/// One member's manifest: every requirement pinned, no `[dependencies]` on
/// the dependency-free crate, no build script.
///
/// # Errors
///
/// [`PolicyError`] when the manifest cannot be read or parsed.
fn check_manifest(
    root: &Path,
    member: &Path,
    violations: &mut Vec<Violation>,
) -> Result<(), PolicyError> {
    let manifest_path = member.join("Cargo.toml");
    let manifest = parse_toml(&manifest_path)?;
    let reported = relative(root, &manifest_path);
    let name = package_name_of(&manifest).unwrap_or_default();
    if name == DEPENDENCY_FREE_CRATE && manifest.get("dependencies").is_some() {
        violations.push(Violation {
            path: reported.clone(),
            line: None,
            rule: DEPENDENCY_FREE_RULE,
            detail: format!(
                "`{DEPENDENCY_FREE_CRATE}` declares `[dependencies]`; by rule it has none"
            ),
        });
    }
    for table in DEPENDENCY_TABLES {
        let Some(dependencies) = manifest.get(*table).and_then(toml::Value::as_table) else {
            continue;
        };
        for (dependency, entry) in dependencies {
            let requirement = entry
                .as_str()
                .or_else(|| entry.get("version").and_then(toml::Value::as_str));
            let Some(requirement) = requirement else {
                continue;
            };
            if !requirement.starts_with('=') {
                violations.push(Violation {
                    path: reported.clone(),
                    line: None,
                    rule: PIN_RULE,
                    detail: format!(
                        "`{table}.{dependency} = \"{requirement}\"` is not an exact `=` pin"
                    ),
                });
            }
        }
    }
    if member.join("build.rs").exists() {
        violations.push(Violation {
            path: relative(root, &member.join("build.rs")),
            line: None,
            rule: BUILD_SCRIPT_RULE,
            detail: "no workspace crate has a build script".to_owned(),
        });
    }
    Ok(())
}

/// The dependencies check.
///
/// # Errors
///
/// [`PolicyError`] when cargo, the allowlist or a manifest cannot be read.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    let resolved = resolved_packages(root)?;
    let allowlisted = allowlisted_packages(root)?;
    let allowlist_path = relative(root, &root.join(ALLOWLIST));
    for name in resolved.difference(&allowlisted) {
        violations.push(Violation {
            path: allowlist_path.clone(),
            line: None,
            rule: UNDECLARED_RULE,
            detail: format!("`{name}` is resolved but not listed under `{ALLOWLIST_HEADING}`"),
        });
    }
    for name in allowlisted.difference(&resolved) {
        violations.push(Violation {
            path: allowlist_path.clone(),
            line: None,
            rule: UNUSED_RULE,
            detail: format!("`{name}` is listed but nothing resolves it"),
        });
    }
    for member in workspace_members(root)? {
        check_manifest(root, &member, &mut violations)?;
    }
    Ok(violations)
}
