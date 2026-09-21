//! The scaffold's own acceptance: module completeness in both directions,
//! README inclusion, `forbid(unsafe_code)` placement, exact pins, the profiles,
//! the emulator optimization, every dispatcher stub, and the gate table Makina
//! runs.
//!
//! The two cases the task lists that cannot live in an in-process test — the
//! musl build under the `regression` profile and the five gates themselves —
//! are the commands the task's done-when runs directly, each under its own
//! deadline; a build that takes minutes cold has no place under a sixty-second
//! test deadline. The cases that build or run the workspace's binaries assume
//! a warm build cache for the same reason, and say so when it is cold.
//!
//! Support functions here are held to the production rules — they return
//! errors rather than panic — because clippy's test relaxations cover a
//! `#[test]` body and nothing else; each test unwraps at its own top level.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use syn::Item;
use xtask::repository_root;

/// The seconds a child process spawned here may run before this test's own
/// watchdog ends it — under the runner's own sixty-second kill, so the
/// message a person reads is this file's. A warm `cargo` invocation or a stub
/// takes well under a second; only a cold cache comes near this.
const COMMAND_DEADLINE_SECONDS: u64 = 45;

/// The status reported for a child the watchdog ended, which is what coreutils
/// `timeout` exits with when its deadline elapsed.
const TIMEOUT_EXIT_STATUS: i32 = 124;

/// The mode of the shim that stands in for `zig`: executable by everyone.
const EXECUTABLE_MODE: u32 = 0o755;

/// The sentence every scaffold stub's documentation carries until its task
/// replaces the body: a subcommand is expected to be a stub exactly while its
/// module still says so, and its case is skipped from the commit that fills it.
const STUB_MARKER: &str = "holds only its documentation and the stub of its entry point";

/// The start of the sentence that names the task filling a module.
const FILLED_BY_PREFIX: &str = "Filled by task `";

/// The workspace members, in manifest order.
const MEMBERS: &[&str] = &[
    "crates/iznik-protocol",
    "crates/iznik-link",
    "crates/iznik-server",
    "crates/iznik-client",
    "crates/iznik-ffi",
    "crates/iznik-cli",
    "crates/iznik-harness",
    "crates/iznik-testkit",
    "crates/iznik-regression",
    "xtask",
];

/// Every workspace crate but `iznik-protocol`, `iznik-harness` and `xtask`:
/// what the two tooling crates may not reach, so that the gate runner builds
/// without the emulator.
const CRATES_TOOLING_MAY_NOT_REACH: &[&str] = &[
    "iznik-link",
    "iznik-server",
    "iznik-client",
    "iznik-ffi",
    "iznik-cli",
    "iznik-testkit",
    "iznik-regression",
];

/// The subcommands each dispatcher routes, keyed by binary name.
const SUBCOMMANDS: &[(&str, &[&str])] = &[
    (
        "xtask",
        &[
            "check",
            "gate",
            "doctor",
            "policy",
            "claims",
            "regression",
            "distribution",
            "header",
            "soak",
        ],
    ),
    (
        "iznik-server",
        &["--stdio", "--daemon", "--foreground", "--stop", "--version"],
    ),
    (
        "iznik",
        &["probe", "state", "tail", "benchmark", "doctor", "uninstall"],
    ),
    ("iznik-regression", &["step"]),
];

/// Every stub: the binary, the command line, and the module whose file
/// carries [`STUB_MARKER`] while the stub stands.
const STUBS: &[(&str, &[&str], &str)] = &[
    ("xtask", &["check"], "xtask/src/gate.rs"),
    ("xtask", &["gate", "format"], "xtask/src/gate.rs"),
    ("xtask", &["doctor"], "xtask/src/doctor.rs"),
    ("xtask", &["policy"], "xtask/src/policy/mod.rs"),
    ("xtask", &["claims", "coverage"], "xtask/src/claims/mod.rs"),
    (
        "xtask",
        &["regression", "images"],
        "xtask/src/regression.rs",
    ),
    (
        "xtask",
        &["distribution", "--target", "x86_64-unknown-linux-musl"],
        "xtask/src/distribution/mod.rs",
    ),
    ("xtask", &["header"], "xtask/src/header.rs"),
    (
        "xtask",
        &["soak", "--duration", "1"],
        "xtask/src/soak/mod.rs",
    ),
    (
        "iznik-server",
        &["--stdio"],
        "crates/iznik-server/src/relay.rs",
    ),
    (
        "iznik-server",
        &["--daemon"],
        "crates/iznik-server/src/daemon/mod.rs",
    ),
    (
        "iznik-server",
        &["--foreground"],
        "crates/iznik-server/src/daemon/mod.rs",
    ),
    (
        "iznik-server",
        &["--stop"],
        "crates/iznik-server/src/daemon/mod.rs",
    ),
    (
        "iznik-server",
        &["--version"],
        "crates/iznik-server/src/daemon/mod.rs",
    ),
    ("iznik", &["probe", "host"], "crates/iznik-cli/src/probe.rs"),
    ("iznik", &["state", "host"], "crates/iznik-cli/src/state.rs"),
    (
        "iznik",
        &["tail", "host", "1"],
        "crates/iznik-cli/src/tail.rs",
    ),
    (
        "iznik",
        &["benchmark", "host"],
        "crates/iznik-cli/src/benchmark.rs",
    ),
    (
        "iznik",
        &["doctor", "host"],
        "crates/iznik-cli/src/doctor.rs",
    ),
    (
        "iznik",
        &["uninstall", "host"],
        "crates/iznik-cli/src/uninstall.rs",
    ),
    (
        "iznik-regression",
        &["step"],
        "crates/iznik-regression/src/step/mod.rs",
    ),
];

/// Runs a program under a deadline of this test's own and returns its captured
/// streams.
///
/// The deadline is a watchdog thread that kills the child, not coreutils
/// `timeout`, which does not exist on every platform this runs on. The status
/// of a child the watchdog ended is the negative of the signal it was sent, so
/// [`TIMEOUT_EXIT_STATUS`] is reported for it, which is what the callers read.
///
/// # Errors
///
/// When the program cannot be spawned.
fn bounded(
    program: &str,
    arguments: &[&str],
    environment: &[(&str, &OsStr)],
) -> Result<Output, String> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(repository_root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in environment {
        command.env(name, value);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("spawning {program}: {error}"))?;
    let watchdog = child.id();
    let finished = Arc::new(AtomicBool::new(false));
    let watching = Arc::clone(&finished);
    let guard = std::thread::spawn(move || {
        let started = Instant::now();
        let until = started
            .checked_add(Duration::from_secs(COMMAND_DEADLINE_SECONDS))
            .unwrap_or(started);
        while Instant::now() < until {
            if watching.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _killed = Command::new("kill")
            .args(["-9", &watchdog.to_string()])
            .status();
        true
    });
    let output = child
        .wait_with_output()
        .map_err(|error| format!("reaping {program}: {error}"))?;
    finished.store(true, Ordering::Release);
    let elapsed = guard.join().unwrap_or(false);
    if elapsed {
        // The exit code stands in the second byte of a wait status, which is
        // what `ExitStatus::from_raw` reads.
        return Ok(Output {
            status: ExitStatus::from_raw(TIMEOUT_EXIT_STATUS << 8),
            stdout: output.stdout,
            stderr: output.stderr,
        });
    }
    Ok(output)
}

/// Why a `cargo` invocation failed, in the words a person needs: a deadline
/// that elapsed means a cold cache, which no test here is allowed to pay for.
fn cargo_failure(what: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(TIMEOUT_EXIT_STATUS) {
        return format!(
            "{what}: the {COMMAND_DEADLINE_SECONDS}-second deadline elapsed; this case assumes a warm build cache; run `cargo build --workspace` first\n{stderr}"
        );
    }
    format!("{what}: {stderr}")
}

/// Reads a file under the repository root.
///
/// # Errors
///
/// When the file cannot be read.
fn read(relative: &str) -> Result<String, String> {
    let path = repository_root().join(relative);
    fs::read_to_string(&path).map_err(|error| format!("reading {}: {error}", path.display()))
}

/// Parses a TOML file under the repository root.
///
/// # Errors
///
/// When the file cannot be read or is not TOML.
fn parse_toml(relative: &str) -> Result<toml::Table, String> {
    toml::from_str(&read(relative)?).map_err(|error| format!("parsing {relative}: {error}"))
}

/// Every `.rs` file under a directory, recursively, in sorted order.
///
/// # Errors
///
/// When a directory cannot be listed.
fn rust_files_under(directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("listing {}: {error}", directory.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("listing {}: {error}", directory.display()))?
            .path();
        if path.is_dir() {
            files.extend(rust_files_under(&path)?);
        } else if path.extension() == Some(OsStr::new("rs")) {
            files.insert(path);
        }
    }
    Ok(files)
}

/// The files the `mod` declarations reachable from `file` name, resolved in
/// `mod.rs` directory style — a child of `src/lib.rs`, `src/main.rs` or
/// `<name>/mod.rs` lives beside that file — and followed recursively; a
/// declaration without a file is recorded under the `.rs` form so the
/// assertion shows it.
///
/// # Errors
///
/// When a file cannot be read or parsed.
fn declared_modules(file: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let text =
        fs::read_to_string(file).map_err(|error| format!("reading {}: {error}", file.display()))?;
    let parsed =
        syn::parse_file(&text).map_err(|error| format!("parsing {}: {error}", file.display()))?;
    let directory = file
        .parent()
        .ok_or_else(|| format!("{} has no directory", file.display()))?;
    let mut declared = BTreeSet::new();
    for item in parsed.items {
        let Item::Mod(module) = item else { continue };
        if module.content.is_some() {
            continue;
        }
        let name = module.ident.to_string();
        let as_directory = directory.join(&name).join("mod.rs");
        let resolved = if as_directory.exists() {
            as_directory
        } else {
            directory.join(format!("{name}.rs"))
        };
        if resolved.exists() {
            declared.extend(declared_modules(&resolved)?);
        }
        declared.insert(resolved);
    }
    Ok(declared)
}

/// Module completeness in both directions: for every crate, the set of `.rs`
/// files under `src/` equals the set reachable from the crate root through
/// `mod` declarations — an orphaned file is simply not compiled and no lint
/// would catch it.
///
/// # Panics
///
/// When a crate's declarations and files differ, naming the crate.
#[test]
fn module_files_and_declarations_agree() {
    for member in MEMBERS {
        let source = repository_root().join(member).join("src");
        let mut declared = BTreeSet::new();
        let mut roots = Vec::new();
        for root in ["lib.rs", "main.rs"] {
            let path = source.join(root);
            if path.exists() {
                declared.extend(declared_modules(&path).expect("the crate root parses"));
                roots.push(path);
            }
        }
        assert!(!roots.is_empty(), "{member}: no crate root under src/");
        let present: BTreeSet<PathBuf> = rust_files_under(&source)
            .expect("src/ is listable")
            .into_iter()
            .filter(|path| !roots.contains(path))
            .collect();
        assert_eq!(
            declared, present,
            "{member}: the modules declared from the crate root and the files under src/ differ"
        );
    }
}

/// Every crate root includes its README and, except `iznik-ffi`'s, forbids
/// unsafe code; a binary's `main.rs` is a crate root too.
///
/// # Panics
///
/// When a crate root lacks either line, naming the file.
#[test]
fn crate_roots_include_readme_and_forbid_unsafe() {
    for member in MEMBERS {
        for root in ["src/lib.rs", "src/main.rs"] {
            let relative = format!("{member}/{root}");
            if !repository_root().join(&relative).exists() {
                continue;
            }
            let text = read(&relative).expect("the crate root is readable");
            assert!(
                text.contains("#![doc = include_str!(\"../README.md\")]"),
                "{relative} does not include its README"
            );
            let forbids = text.contains("#![forbid(unsafe_code)]");
            if *member == "crates/iznik-ffi" {
                assert!(
                    !forbids,
                    "{relative} is the C boundary and must be able to use unsafe"
                );
            } else {
                assert!(forbids, "{relative} does not forbid unsafe code");
            }
        }
    }
}

/// Every README begins with `# <crate name>` and every fenced block in it
/// carries a language tag other than `rust`, so no fence is a doctest that
/// nextest never runs.
///
/// # Panics
///
/// When a README fails either rule, naming the file.
#[test]
fn readmes_begin_with_heading_and_tag_fences() {
    for member in MEMBERS {
        let manifest = parse_toml(&format!("{member}/Cargo.toml")).expect("the manifest parses");
        let name = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .expect("package name");
        let relative = format!("{member}/README.md");
        let readme = read(&relative).expect("the README is readable");
        assert_eq!(
            readme.lines().next(),
            Some(format!("# {name}").as_str()),
            "{relative} does not begin with the crate heading"
        );
        let mut inside_fence = false;
        for line in readme.lines() {
            let Some(rest) = line.strip_prefix("```") else {
                continue;
            };
            if !inside_fence {
                let tag = rest.trim();
                assert!(!tag.is_empty(), "{relative}: an untagged fence");
                assert_ne!(tag, "rust", "{relative}: a fence tagged rust is a doctest");
            }
            inside_fence = !inside_fence;
        }
        assert!(!inside_fence, "{relative}: an unclosed fence");
    }
}

/// The dependency tables of a manifest, development and build dependencies
/// included, as `(table, name, entry)`.
fn dependency_entries(manifest: &toml::Table) -> Vec<(String, String, toml::Value)> {
    let mut entries = Vec::new();
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(dependencies) = manifest.get(table).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, entry) in dependencies {
            entries.push((table.to_owned(), name.clone(), entry.clone()));
        }
    }
    entries
}

/// Every version requirement in every workspace manifest, development
/// dependencies included, is an exact `=` pin; a workspace crate is named by
/// path and carries no requirement.
///
/// # Panics
///
/// When a requirement is not a pin, naming the crate and the dependency.
#[test]
fn version_requirements_are_exact_pins() {
    for member in MEMBERS {
        let manifest = parse_toml(&format!("{member}/Cargo.toml")).expect("the manifest parses");
        for (table, name, entry) in dependency_entries(&manifest) {
            let requirement = entry
                .as_str()
                .or_else(|| entry.get("version").and_then(toml::Value::as_str));
            match requirement {
                Some(requirement) => assert!(
                    requirement.starts_with('='),
                    "{member}: {table}.{name} = {requirement} is not an exact pin"
                ),
                None => assert!(
                    entry.get("path").is_some(),
                    "{member}: {table}.{name} has neither a version nor a path"
                ),
            }
        }
    }
}

/// `iznik-protocol` declares no `[dependencies]`, and no workspace crate has
/// a `build.rs`.
///
/// # Panics
///
/// When either rule is broken, naming the crate.
#[test]
fn protocol_without_dependencies_and_no_build_scripts() {
    let protocol = parse_toml("crates/iznik-protocol/Cargo.toml").expect("the manifest parses");
    assert!(
        protocol.get("dependencies").is_none(),
        "iznik-protocol declares dependencies"
    );
    for member in MEMBERS {
        assert!(
            !repository_root().join(member).join("build.rs").exists(),
            "{member} has a build script"
        );
    }
}

/// `cargo metadata` for the workspace, resolved under the lockfile.
///
/// # Errors
///
/// When cargo cannot be run, fails, or prints something other than JSON.
fn cargo_metadata() -> Result<serde_json::Value, String> {
    let output = bounded(
        "cargo",
        &["metadata", "--format-version", "1", "--locked"],
        &[],
    )?;
    if !output.status.success() {
        return Err(cargo_failure("cargo metadata", &output));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("cargo metadata: {error}"))
}

/// The `name` of the package with `id` in the metadata's package list.
fn package_name(packages: &[serde_json::Value], id: &str) -> Option<String> {
    packages
        .iter()
        .find(|candidate| candidate.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .and_then(|candidate| candidate.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

/// The resolve-graph node of the package with `id`.
fn resolve_node<'metadata>(
    nodes: &'metadata [serde_json::Value],
    id: &str,
) -> Option<&'metadata serde_json::Value> {
    nodes
        .iter()
        .find(|node| node.get("id").and_then(serde_json::Value::as_str) == Some(id))
}

/// Whether a resolve-graph edge is carried by a normal or build dependency
/// rather than a development one only.
fn carried_outside_tests(dependency: &serde_json::Value) -> bool {
    dependency
        .get("dep_kinds")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|kinds| {
            kinds.iter().any(|kind| {
                let kind = kind.get("kind");
                kind.is_none_or(serde_json::Value::is_null)
                    || kind.and_then(serde_json::Value::as_str) == Some("build")
            })
        })
}

/// The names of every package reachable from `package` through normal and
/// build dependencies — never development ones.
///
/// # Errors
///
/// When the metadata lacks the package or a node it names.
fn normal_dependency_closure(
    metadata: &serde_json::Value,
    package: &str,
) -> Result<BTreeSet<String>, String> {
    let packages = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or("no packages in the metadata")?;
    let nodes = metadata
        .get("resolve")
        .and_then(|resolve| resolve.get("nodes"))
        .and_then(serde_json::Value::as_array)
        .ok_or("no resolve graph in the metadata")?;
    let start = packages
        .iter()
        .find(|candidate| {
            candidate.get("name").and_then(serde_json::Value::as_str) == Some(package)
        })
        .and_then(|candidate| candidate.get("id").and_then(serde_json::Value::as_str))
        .ok_or_else(|| format!("{package} is not a workspace package"))?;
    let mut pending = vec![start.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        let node =
            resolve_node(nodes, &id).ok_or_else(|| format!("{id} is not in the resolve graph"))?;
        let dependencies = node
            .get("deps")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("{id} has no deps"))?;
        for dependency in dependencies {
            if !carried_outside_tests(dependency) {
                continue;
            }
            let dependency_id = dependency
                .get("pkg")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("a dependency of {id} has no pkg"))?;
            let name = package_name(packages, dependency_id)
                .ok_or_else(|| format!("{dependency_id} is not in the metadata"))?;
            if seen.insert(name) {
                pending.push(dependency_id.to_owned());
            }
        }
    }
    Ok(seen)
}

/// `iznik-harness` and `xtask` depend on neither `libghostty-vt` nor any
/// product crate but `iznik-protocol`, asserted from `cargo metadata`: the
/// only build script in the workspace's tree that runs Zig is the emulator's,
/// so this is what makes the tool whose job is to say that Zig is missing
/// buildable without it.
///
/// # Panics
///
/// When either crate reaches the emulator or a product crate, naming both.
#[test]
fn tooling_crates_reach_neither_emulator_nor_product() {
    let metadata = cargo_metadata().expect("cargo metadata");
    for package in ["iznik-harness", "xtask"] {
        let closure = normal_dependency_closure(&metadata, package).expect("the closure resolves");
        for emulator in ["libghostty-vt", "libghostty-vt-sys"] {
            assert!(!closure.contains(emulator), "{package} reaches {emulator}");
        }
        for product in CRATES_TOOLING_MAY_NOT_REACH {
            assert!(!closure.contains(*product), "{package} reaches {product}");
        }
    }
}

/// The root manifest declares `[profile.regression]` and `[profile.release]`
/// with exactly the fields the architecture lists — plus `strip = false` on
/// the regression profile, without which the `strip = true` it inherits from
/// release would discard the line tables its `debug` field asks for.
///
/// # Panics
///
/// When a profile differs from the architecture's, showing both.
#[test]
fn profiles_match_architecture() {
    let manifest = parse_toml("Cargo.toml").expect("the root manifest parses");
    let profiles = manifest
        .get("profile")
        .and_then(toml::Value::as_table)
        .expect("profiles");
    let expected_regression: toml::Table = toml::from_str(
        "inherits = \"release\"\nlto = false\ncodegen-units = 16\nincremental = true\ndebug = \"line-tables-only\"\ndebug-assertions = true\nstrip = false\n",
    )
    .expect("expected regression profile");
    let expected_release: toml::Table = toml::from_str(
        "strip = true\ndebug = false\nlto = \"fat\"\ncodegen-units = 1\npanic = \"abort\"\n",
    )
    .expect("expected release profile");
    assert_eq!(
        profiles.get("regression").and_then(toml::Value::as_table),
        Some(&expected_regression),
        "the regression profile"
    );
    assert_eq!(
        profiles.get("release").and_then(toml::Value::as_table),
        Some(&expected_release),
        "the release profile"
    );
}

/// `.cargo/config.toml` forces the emulator's optimized build for every
/// profile, so no test runs a slower emulator than the product ships.
///
/// # Panics
///
/// When the variable is absent or set to anything else.
#[test]
fn cargo_configuration_forces_emulator_optimization() {
    let configuration = parse_toml(".cargo/config.toml").expect("the cargo configuration parses");
    let optimize = configuration
        .get("env")
        .and_then(|environment| environment.get("LIBGHOSTTY_VT_SYS_OPTIMIZE"))
        .and_then(toml::Value::as_str);
    assert_eq!(optimize, Some("ReleaseFast"), "LIBGHOSTTY_VT_SYS_OPTIMIZE");
}

/// The target directory this test was built into and the profile's name, from
/// the path cargo hands the test for `xtask`'s own binary:
/// `<target>/<profile>/xtask`.
fn target_directory() -> Option<(PathBuf, String)> {
    let xtask_binary = PathBuf::from(env!("CARGO_BIN_EXE_xtask"));
    let profile_directory = xtask_binary.parent()?;
    let profile = profile_directory.file_name()?.to_str()?.to_owned();
    Some((profile_directory.parent()?.to_path_buf(), profile))
}

/// The directory the workspace's binaries are built into for the profile this
/// test was built under, with the three binaries other packages own built
/// through cargo — `xtask`'s own is where cargo says it is, and a binary a
/// test guesses the path of is a stale one. Warm, the build is a lock and a
/// fingerprint check; cold, it is minutes, which the failure message says.
///
/// # Errors
///
/// When the target directory cannot be told from the test's own path, or the
/// build fails.
fn built_binaries() -> Result<PathBuf, String> {
    let (target, profile) = target_directory().ok_or("the target directory is unknown")?;
    let mut arguments = vec![
        "build",
        "--locked",
        "--package",
        "iznik-server",
        "--package",
        "iznik-cli",
        "--package",
        "iznik-regression",
    ];
    if profile != "debug" {
        arguments.extend(["--profile", profile.as_str()]);
    }
    let output = bounded(
        "cargo",
        &arguments,
        &[("CARGO_TARGET_DIR", target.as_os_str())],
    )?;
    if !output.status.success() {
        return Err(cargo_failure("building the binaries", &output));
    }
    Ok(target.join(profile))
}

/// Runs one of the workspace's binaries by name from the built directory.
///
/// # Errors
///
/// When the binary cannot be spawned.
fn run_binary(binaries: &Path, binary: &str, arguments: &[&str]) -> Result<Output, String> {
    let path = binaries.join(binary);
    let program = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    bounded(program, arguments, &[])
}

/// The task a module's documentation says fills it, while the module is still
/// a scaffold stub; `None` once the stub has been replaced.
///
/// # Errors
///
/// When the module cannot be read, or carries the marker but names no task.
fn stub_task(module: &str) -> Result<Option<String>, String> {
    let text = read(module)?;
    if !text.contains(STUB_MARKER) {
        return Ok(None);
    }
    let task = text
        .split(FILLED_BY_PREFIX)
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .ok_or_else(|| format!("{module} names no filling task"))?;
    Ok(Some(task.to_owned()))
}

/// Every dispatcher subcommand whose module is still a stub exits 2 with its
/// stub line on stderr and nothing on stdout, the two streams captured
/// separately; a subcommand whose task has landed is that task's to test.
///
/// # Panics
///
/// When a stub's status or streams differ, naming the binary and subcommand.
#[test]
fn stubs_exit_two_with_line_on_stderr() {
    let binaries = built_binaries().expect("the binaries build");
    for (binary, arguments, module) in STUBS {
        let Some(task) = stub_task(module).expect("the module is readable") else {
            continue;
        };
        let output = run_binary(&binaries, binary, arguments).expect("the binary runs");
        let subcommand = arguments.first().expect("a subcommand");
        assert_eq!(
            output.status.code(),
            Some(i32::from(xtask::USAGE_EXIT_CODE)),
            "{binary} {subcommand}: exit status"
        );
        assert!(
            output.stdout.is_empty(),
            "{binary} {subcommand} wrote to stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("{subcommand}: not implemented until task {task}\n"),
            "{binary} {subcommand}: stderr differs while {module} still carries the scaffold's stub marker"
        );
    }
}

/// A first argument the dispatcher does not know, or none at all, exits 2
/// naming the known ones on stderr; `--help` names them on stdout and exits 0.
///
/// # Panics
///
/// When a dispatcher's status or streams differ, naming the binary.
#[test]
fn unknown_argument_names_subcommands() {
    let binaries = built_binaries().expect("the binaries build");
    for (binary, subcommands) in SUBCOMMANDS {
        for arguments in [&["no-such-subcommand"][..], &[][..]] {
            let output = run_binary(&binaries, binary, arguments).expect("the binary runs");
            assert_eq!(
                output.status.code(),
                Some(i32::from(xtask::USAGE_EXIT_CODE)),
                "{binary} {arguments:?}: exit status"
            );
            assert!(
                output.stdout.is_empty(),
                "{binary} {arguments:?} wrote to stdout"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            for subcommand in *subcommands {
                assert!(
                    stderr.contains(subcommand),
                    "{binary} {arguments:?}: stderr does not name {subcommand}: {stderr}"
                );
            }
        }
        let help = run_binary(&binaries, binary, &["--help"]).expect("the binary runs");
        assert_eq!(help.status.code(), Some(0), "{binary} --help: exit status");
        assert!(help.stderr.is_empty(), "{binary} --help wrote to stderr");
        let stdout = String::from_utf8_lossy(&help.stdout);
        for subcommand in *subcommands {
            assert!(
                stdout.contains(subcommand),
                "{binary} --help does not list {subcommand}: {stdout}"
            );
        }
    }
}

/// A scratch directory of this process's own under the system's temporary
/// directory, removed when dropped so that a failing assertion leaves nothing
/// behind.
#[derive(Debug)]
struct Scratch {
    /// The directory's path.
    path: PathBuf,
}

impl Scratch {
    /// Creates the directory.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created.
    fn new(name: &str) -> Result<Scratch, String> {
        let path = env::temp_dir().join(format!("iznik-skeleton-{name}-{}", std::process::id()));
        fs::create_dir_all(&path)
            .map_err(|error| format!("creating {}: {error}", path.display()))?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap_or_default();
    }
}

/// `xtask claims verify` exits 0 against this tree, where no registry exists
/// yet, and exits non-zero naming `claims-registry` against a root that
/// contains `regression/claims/`: the fifth gate's bootstrap form, which
/// stands exactly while the claims module is still a stub.
///
/// # Panics
///
/// When either run's status or streams differ.
#[test]
fn claims_verify_bootstraps_until_registry() {
    if stub_task("xtask/src/claims/mod.rs")
        .expect("the claims module is readable")
        .is_none()
    {
        return;
    }
    let binaries = built_binaries().expect("the binaries build");
    let against_this_tree =
        run_binary(&binaries, "xtask", &["claims", "verify"]).expect("xtask runs");
    assert_eq!(
        against_this_tree.status.code(),
        Some(0),
        "claims verify against this tree"
    );
    assert!(
        against_this_tree.stdout.is_empty(),
        "claims verify wrote to stdout"
    );
    assert!(
        against_this_tree.stderr.is_empty(),
        "claims verify wrote to stderr"
    );

    let scratch = Scratch::new("claims").expect("a scratch directory");
    fs::create_dir_all(scratch.path.join("regression").join("claims")).expect("a claims directory");
    let root = scratch.path.to_str().expect("a UTF-8 path");
    let against_a_registry =
        run_binary(&binaries, "xtask", &["claims", "verify", "--root", root]).expect("xtask runs");
    assert_ne!(
        against_a_registry.status.code(),
        Some(0),
        "claims verify against a registry"
    );
    assert!(
        String::from_utf8_lossy(&against_a_registry.stderr).contains("claims-registry"),
        "claims verify does not name the task: {}",
        String::from_utf8_lossy(&against_a_registry.stderr)
    );
}

/// `cargo build --package xtask` completes without invoking Zig, asserted
/// with a `PATH` on which `zig` is a program that fails loudly — and, since a
/// warm cache runs no build script at all, with the verbose unit list, which
/// names the emulator's crate whenever it is in the build whether or not its
/// script ran.
///
/// # Panics
///
/// When the build fails, the shim reports that it was run, or the unit list
/// names the emulator.
#[test]
fn xtask_builds_without_invoking_zig() {
    let scratch = Scratch::new("zig").expect("a scratch directory");
    let zig = scratch.path.join("zig");
    fs::write(
        &zig,
        "#!/bin/sh\nprintf 'zig must not be invoked by a build of xtask\\n' >&2\nexit 1\n",
    )
    .expect("writing the zig shim");
    fs::set_permissions(&zig, fs::Permissions::from_mode(EXECUTABLE_MODE))
        .expect("an executable shim");
    let mut path = scratch.path.clone().into_os_string();
    if let Some(existing) = env::var_os("PATH") {
        path.push(":");
        path.push(existing);
    }
    let (target, _profile) = target_directory().expect("the target directory");
    let output = bounded(
        "cargo",
        &["build", "--locked", "--verbose", "--package", "xtask"],
        &[
            ("PATH", path.as_os_str()),
            ("CARGO_TARGET_DIR", target.as_os_str()),
        ],
    )
    .expect("cargo runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}",
        cargo_failure("building xtask with zig hidden", &output)
    );
    assert!(
        !stderr.contains("zig must not be invoked"),
        "the build invoked zig: {stderr}"
    );
    assert!(
        !stderr.contains("libghostty-vt-sys"),
        "the build of xtask includes the emulator: {stderr}"
    );
}

/// `.makina/config.toml` names exactly the five gates in the order the
/// architecture tabulates, so a task that passes locally is a task that lands.
///
/// # Panics
///
/// When the gate names or their order differ.
#[test]
fn makina_gates_match_order() {
    let configuration = parse_toml(".makina/config.toml").expect("the Makina configuration parses");
    let names: Vec<&str> = configuration
        .get("gates")
        .and_then(toml::Value::as_array)
        .expect("gates")
        .iter()
        .map(|gate| {
            gate.get("name")
                .and_then(toml::Value::as_str)
                .expect("a gate name")
        })
        .collect();
    assert_eq!(
        names,
        ["format", "lint", "documentation", "test", "claims"],
        "the gates Makina runs"
    );
}
