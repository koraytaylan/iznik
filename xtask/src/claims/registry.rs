//! Loading and validating every claims file under `regression/claims/`.
//!
//! A claims file is `regression/claims/<task-id>.toml`, and each `[[claim]]`
//! has a stable `id`, a present-tense `statement`, and exactly one proof — a
//! `scenario` under the same task, or a `test` with a `because` that says why
//! a container adds nothing, or a deferred `display` measurement record. Loading validates the whole set at once: ids are
//! unique across the registry, every proof is well formed, every file is named
//! for a real task under `docs/plans/`, every scenario proof names a scenario
//! that exists, every test proof names a test target a workspace package
//! declares, and no scenario of a registered task names a claim the registry
//! does not declare. A task that has no claims file is not in the registry and
//! its scenarios are not read, which is how the tasks that landed before the
//! registry stay exempt from it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::claims::test_source;

/// The registry's directory, under the repository root.
pub const CLAIMS_DIRECTORY: &str = "regression/claims";

/// The scenarios' directory, under the repository root.
const SCENARIOS_DIRECTORY: &str = "regression/scenarios";

/// Where the task files whose ids name the claims files live.
const TASKS_GLOB_ROOT: &str = "docs/plans";

/// The manifest of the workspace, and of each package in it.
const MANIFEST: &str = "Cargo.toml";

/// Where cargo finds a package's integration tests, one target per file.
const TESTS_DIRECTORY: &str = "tests";

/// What separates the package, the test target and the test in a test proof.
const TEST_PATH_SEPARATOR: &str = "::";

/// The parts a test proof is split into: the package, the test target, and
/// the rest, which is the test's own path and may itself hold separators.
const TEST_PATH_PARTS: usize = 3;

/// A validated claim: what a task asserts about runtime behavior and the one
/// proof that establishes it.
#[derive(Clone, Debug)]
pub struct Claim {
    /// The task that declares it — the claims file's stem.
    pub task: String,
    /// The stable id, unique across the registry.
    pub id: String,
    /// The one-sentence, present-tense statement.
    pub statement: String,
    /// The single proof.
    pub proof: Proof,
    /// The operating system the proof needs, when it needs a named one.
    pub platform: Option<String>,
    /// The cargo profile the proof runs under, when it is not the default.
    pub profile: Option<String>,
}

/// The one proof of a claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proof {
    /// A scenario under the task, by file stem, at
    /// `regression/scenarios/<task>/<name>.toml`.
    Scenario {
        /// The scenario's file stem.
        name: String,
    },
    /// A display-bound measurement kept explicitly deferred by the automated gate.
    Display {
        /// Existing Markdown record under `docs/notes/`, relative to the repository.
        record: String,
        /// Why native display hardware and manual measurement are required.
        because: String,
    },
    /// A test, by its `package::binary::test` path, with the reason a container
    /// adds nothing to it.
    Test {
        /// The nextest path of the test.
        name: String,
        /// Why the claim needs no container to prove.
        because: String,
    },
}

/// The whole registry: every claim, its files read in sorted order and each
/// file's claims in the order written.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    /// Every claim, files sorted by name and each file's claims in order.
    claims: Vec<Claim>,
}

impl Registry {
    /// Every claim in the registry.
    #[must_use]
    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    /// Every claim a task declares, in the order written.
    pub fn claims_of<'registry>(
        &'registry self,
        task: &'registry str,
    ) -> impl Iterator<Item = &'registry Claim> {
        self.claims.iter().filter(move |claim| claim.task == task)
    }

    /// Every task that declares a claim, sorted and unique.
    #[must_use]
    pub fn tasks(&self) -> Vec<String> {
        let mut tasks: Vec<String> = self.claims.iter().map(|claim| claim.task.clone()).collect();
        tasks.sort();
        tasks.dedup();
        tasks
    }
}

/// A claims file as written: a list of `[[claim]]` tables and nothing else.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimFile {
    /// The `[[claim]]` tables, in the order written.
    #[serde(default)]
    claim: Vec<RawClaim>,
}

/// One `[[claim]]` before its one-proof rule is checked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaim {
    /// The stable id.
    id: String,
    /// The present-tense statement.
    statement: String,
    /// A scenario proof, by scenario file stem.
    scenario: Option<String>,
    /// A test proof, by `package::binary::test` path.
    test: Option<String>,
    /// A deferred display measurement record under `docs/notes/`.
    display: Option<String>,
    /// Why a test needs no container or a display measurement cannot be automated here.
    because: Option<String>,
    /// The operating system the proof needs, when it needs a named one.
    platform: Option<String>,
    /// The cargo profile the proof runs under, when not the default.
    profile: Option<String>,
}

impl RawClaim {
    /// The claim's proof, or why it is not exactly one well-formed proof.
    ///
    /// # Errors
    ///
    /// [`RegistryError::BothProofs`] or [`RegistryError::NeitherProof`] when the
    /// count is not one, and [`RegistryError::TestWithoutBecause`] when a test
    /// proof has no reason; `DisplayRecord` when a display measurement has no reason.
    fn proof(&self, task: &str) -> Result<Proof, RegistryError> {
        match (
            self.scenario.as_ref(),
            self.test.as_ref(),
            self.display.as_ref(),
        ) {
            (None, None, None) => Err(RegistryError::NeitherProof {
                task: task.to_owned(),
                id: self.id.clone(),
            }),
            (Some(name), None, None) => Ok(Proof::Scenario { name: name.clone() }),
            (None, Some(name), None) => {
                let because =
                    self.because
                        .clone()
                        .ok_or_else(|| RegistryError::TestWithoutBecause {
                            task: task.to_owned(),
                            id: self.id.clone(),
                        })?;
                Ok(Proof::Test {
                    name: name.clone(),
                    because,
                })
            }
            (None, None, Some(record)) => {
                let because = self
                    .because
                    .as_ref()
                    .filter(|reason| !reason.trim().is_empty())
                    .ok_or_else(|| RegistryError::DisplayRecord {
                        task: task.to_owned(),
                        id: self.id.clone(),
                        reason: "a display measurement requires a nonempty `because`".to_owned(),
                    })?;
                Ok(Proof::Display {
                    record: record.clone(),
                    because: because.clone(),
                })
            }
            _ => Err(RegistryError::BothProofs {
                task: task.to_owned(),
                id: self.id.clone(),
            }),
        }
    }
}

/// The `claims` array of a scenario, read leniently — a scenario the registry
/// does not otherwise concern itself with.
#[derive(Debug, Deserialize)]
struct ScenarioClaims {
    /// The claim ids the scenario proves.
    #[serde(default)]
    claims: Vec<String>,
}

/// Why a registry could not be loaded, each naming what and where.
#[derive(Debug)]
pub enum RegistryError {
    /// A file could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// A claims file did not parse.
    Toml {
        /// The file.
        path: PathBuf,
        /// What the parser said.
        source: Box<toml::de::Error>,
    },
    /// A claims file is named for no task under `docs/plans/`.
    UnknownTask {
        /// The file.
        path: PathBuf,
    },
    /// Two claims share an id.
    DuplicateId {
        /// The shared id.
        id: String,
        /// The file that declared it first.
        first: PathBuf,
        /// The file that declared it again.
        second: PathBuf,
    },
    /// A test proof has no `because`.
    TestWithoutBecause {
        /// The task.
        task: String,
        /// The claim.
        id: String,
    },
    /// A deferred display record is missing, malformed or lacks a reason.
    DisplayRecord {
        /// The task.
        task: String,
        /// The claim.
        id: String,
        /// The invalid record requirement.
        reason: String,
    },
    /// A claim names more than one proof category.
    BothProofs {
        /// The task.
        task: String,
        /// The claim.
        id: String,
    },
    /// A claim names no proof category.
    NeitherProof {
        /// The task.
        task: String,
        /// The claim.
        id: String,
    },
    /// A scenario proof names a scenario file that does not exist.
    ScenarioFileMissing {
        /// The task.
        task: String,
        /// The claim.
        id: String,
        /// The scenario file the proof named.
        path: PathBuf,
    },
    /// A test proof names no test target a workspace package declares: a
    /// package that is not a member, a binary with no `tests/<binary>.rs` or
    /// `[[test]]`, or a path that is not `package::binary::test`.
    TestTargetMissing {
        /// The task.
        task: String,
        /// The claim.
        id: String,
        /// The test proof as written.
        test: String,
    },
    /// A test proof names a test target that exists but whose source defines
    /// no function of the test's name.
    TestFunctionMissing {
        /// The task.
        task: String,
        /// The claim.
        id: String,
        /// The test proof as written.
        test: String,
        /// The target's root file, which the scan started from.
        file: PathBuf,
    },
    /// A scenario of a registered task names a claim the registry does not
    /// declare.
    ScenarioClaimUndeclared {
        /// The task whose scenario it is.
        task: String,
        /// The scenario file.
        scenario: PathBuf,
        /// The claim it named.
        claim: String,
    },
}

impl Display for RegistryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Read { path, source } => {
                write!(formatter, "reading {}: {source}", path.display())
            }
            RegistryError::Toml { path, source } => {
                write!(formatter, "parsing {}: {source}", path.display())
            }
            RegistryError::UnknownTask { path } => write!(
                formatter,
                "{} is named for no task under {TASKS_GLOB_ROOT}",
                path.display()
            ),
            RegistryError::DuplicateId { id, first, second } => write!(
                formatter,
                "claim `{id}` is declared in both {} and {}",
                first.display(),
                second.display()
            ),
            RegistryError::TestWithoutBecause { task, id } => write!(
                formatter,
                "claim `{id}` of task `{task}` has a test proof but no `because`"
            ),
            RegistryError::DisplayRecord { task, id, reason } => write!(
                formatter,
                "claim `{id}` of task `{task}` has an invalid display record: {reason}"
            ),
            RegistryError::BothProofs { task, id } => write!(
                formatter,
                "claim `{id}` of task `{task}` names more than one of scenario, test and display"
            ),
            RegistryError::NeitherProof { task, id } => write!(
                formatter,
                "claim `{id}` of task `{task}` names none of scenario, test and display"
            ),
            RegistryError::ScenarioFileMissing { task, id, path } => write!(
                formatter,
                "claim `{id}` of task `{task}` names scenario {}, which does not exist",
                path.display()
            ),
            RegistryError::TestTargetMissing { task, id, test } => write!(
                formatter,
                "claim `{id}` of task `{task}` names test `{test}`, but no workspace package declares that test target"
            ),
            RegistryError::TestFunctionMissing {
                task,
                id,
                test,
                file,
            } => write!(
                formatter,
                "claim `{id}` of task `{task}` names test `{test}`, but {} and the modules it pulls in define no such function",
                file.display()
            ),
            RegistryError::ScenarioClaimUndeclared {
                task,
                scenario,
                claim,
            } => write!(
                formatter,
                "scenario {} of task `{task}` names claim `{claim}`, which is declared nowhere",
                scenario.display()
            ),
        }
    }
}

impl std::error::Error for RegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RegistryError::Read { source, .. } => Some(source),
            RegistryError::Toml { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Loads and validates the whole registry under `root`.
///
/// # Errors
///
/// [`RegistryError`] naming the first inconsistency: an unreadable or
/// unparseable file, a file named for no task, a duplicate id, a malformed
/// proof, a scenario proof with no scenario, or a scenario of a registered task
/// naming an undeclared claim, or a test proof naming no test target or no
/// function of its target.
pub fn load(root: &Path) -> Result<Registry, RegistryError> {
    let tasks = task_ids(root);
    let mut claims: Vec<Claim> = Vec::new();
    let mut origin: BTreeMap<String, PathBuf> = BTreeMap::new();
    for file in claim_files(&root.join(CLAIMS_DIRECTORY))? {
        let task = stem(&file);
        if !tasks.contains(&task) {
            return Err(RegistryError::UnknownTask { path: file });
        }
        parse_file(&file, &task, &mut claims, &mut origin)?;
    }
    check_scenario_proofs(root, &claims)?;
    check_test_targets(root, &claims)?;
    check_display_records(root, &claims)?;
    check_scenario_claims(root, &claims)?;
    Ok(Registry { claims })
}

/// Parses one claims file, appending its claims and recording their origin for
/// duplicate detection.
///
/// # Errors
///
/// [`RegistryError`] when the file cannot be read or parsed, a claim's proof is
/// malformed, or a claim's id is already taken.
fn parse_file(
    file: &Path,
    task: &str,
    claims: &mut Vec<Claim>,
    origin: &mut BTreeMap<String, PathBuf>,
) -> Result<(), RegistryError> {
    let text = std::fs::read_to_string(file).map_err(|source| RegistryError::Read {
        path: file.to_path_buf(),
        source,
    })?;
    let parsed: ClaimFile = toml::from_str(&text).map_err(|source| RegistryError::Toml {
        path: file.to_path_buf(),
        source: Box::new(source),
    })?;
    for raw in parsed.claim {
        let proof = raw.proof(task)?;
        if let Some(first) = origin.get(&raw.id) {
            return Err(RegistryError::DuplicateId {
                id: raw.id,
                first: first.clone(),
                second: file.to_path_buf(),
            });
        }
        origin.insert(raw.id.clone(), file.to_path_buf());
        claims.push(Claim {
            task: task.to_owned(),
            id: raw.id,
            statement: raw.statement,
            proof,
            platform: raw.platform,
            profile: raw.profile,
        });
    }
    Ok(())
}

/// Every scenario proof names a scenario file that exists.
///
/// # Errors
///
/// [`RegistryError::ScenarioFileMissing`] for the first proof whose scenario is
/// absent.
fn check_scenario_proofs(root: &Path, claims: &[Claim]) -> Result<(), RegistryError> {
    for claim in claims {
        if let Proof::Scenario { name } = &claim.proof {
            let path = root
                .join(SCENARIOS_DIRECTORY)
                .join(&claim.task)
                .join(format!("{name}.toml"));
            if !path.is_file() {
                return Err(RegistryError::ScenarioFileMissing {
                    task: claim.task.clone(),
                    id: claim.id.clone(),
                    path,
                });
            }
        }
    }
    Ok(())
}

/// Every test proof names a test target that exists: its package is a
/// workspace member, and that member has `tests/<binary>.rs`,
/// `tests/<binary>/main.rs` or a `[[test]]` of that name. And the target's
/// source defines the test function, which [`test_source::defines_test`]
/// scans for; a target without the libtest harness names its tests at run
/// time, so its tests are not looked for. A proof naming a target or a
/// function that is not there would otherwise select nothing and be reported
/// only as a missing verdict.
///
/// # Errors
///
/// [`RegistryError::TestTargetMissing`] for the first proof that names no
/// target, [`RegistryError::TestFunctionMissing`] for the first whose target
/// defines no such function.
fn check_test_targets(root: &Path, claims: &[Claim]) -> Result<(), RegistryError> {
    let packages = workspace_packages(root);
    for claim in claims {
        let Proof::Test { name, .. } = &claim.proof else {
            continue;
        };
        let mut parts = name.splitn(TEST_PATH_PARTS, TEST_PATH_SEPARATOR);
        let target = match (parts.next(), parts.next(), parts.next()) {
            (Some(package), Some(binary), Some(test)) if !test.is_empty() => packages
                .get(package)
                .and_then(|directory| test_target(directory, binary))
                .map(|source| (source, test)),
            _ => None,
        };
        match target {
            None => {
                return Err(RegistryError::TestTargetMissing {
                    task: claim.task.clone(),
                    id: claim.id.clone(),
                    test: name.clone(),
                });
            }
            Some((TargetSource::File(file), test)) if !test_source::defines_test(&file, test) => {
                return Err(RegistryError::TestFunctionMissing {
                    task: claim.task.clone(),
                    id: claim.id.clone(),
                    test: name.clone(),
                    file,
                });
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Where a test target's tests are written, as far as the function check
/// needs to know.
enum TargetSource {
    /// The target's root file, which defines its tests or pulls in the
    /// modules that do.
    File(PathBuf),
    /// A target whose tests are not functions to look for: one without the
    /// libtest harness, or one whose root file is not where it says.
    Unchecked,
}

/// Every workspace member's package name and directory, read from the root
/// manifest's `members` and each member's own manifest. A root without a
/// workspace manifest has no packages.
fn workspace_packages(root: &Path) -> BTreeMap<String, PathBuf> {
    let members: Vec<String> = read_table(&root.join(MANIFEST))
        .and_then(|manifest| {
            manifest
                .get("workspace")?
                .get("members")?
                .as_array()
                .map(|members| {
                    members
                        .iter()
                        .filter_map(|member| member.as_str().map(str::to_owned))
                        .collect()
                })
        })
        .unwrap_or_default();
    members
        .into_iter()
        .filter_map(|member| {
            let directory = root.join(member);
            let manifest = read_table(&directory.join(MANIFEST))?;
            let name = manifest.get("package")?.get("name")?.as_str()?.to_owned();
            Some((name, directory))
        })
        .collect()
}

/// The test target named `binary` a package directory declares, if any: a
/// `[[test]]` of that name, or else `tests/<binary>.rs` or
/// `tests/<binary>/main.rs`.
fn test_target(directory: &Path, binary: &str) -> Option<TargetSource> {
    let tests = directory.join(TESTS_DIRECTORY);
    let declared = read_table(&directory.join(MANIFEST)).and_then(|manifest| {
        manifest
            .get("test")?
            .as_array()?
            .iter()
            .find(|target| target.get("name").and_then(toml::Value::as_str) == Some(binary))
            .cloned()
    });
    if let Some(target) = declared {
        if target.get("harness").and_then(toml::Value::as_bool) == Some(false) {
            return Some(TargetSource::Unchecked);
        }
        let file = target
            .get("path")
            .and_then(toml::Value::as_str)
            .map_or_else(
                || tests.join(format!("{binary}.rs")),
                |path| directory.join(path),
            );
        return Some(if file.is_file() {
            TargetSource::File(file)
        } else {
            TargetSource::Unchecked
        });
    }
    [
        tests.join(format!("{binary}.rs")),
        tests.join(binary).join("main.rs"),
    ]
    .into_iter()
    .find(|file| file.is_file())
    .map(TargetSource::File)
}

/// A TOML file read as a table, or nothing when it cannot be read or parsed.
fn read_table(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

/// No scenario of a registered task names a claim the registry does not
/// declare. A task with no claims file is not registered, so its scenarios are
/// not read.
///
/// # Errors
///
/// [`RegistryError::ScenarioClaimUndeclared`] for the first such scenario.
fn check_scenario_claims(root: &Path, claims: &[Claim]) -> Result<(), RegistryError> {
    let declared: BTreeSet<&str> = claims.iter().map(|claim| claim.id.as_str()).collect();
    let mut registered: Vec<&str> = claims.iter().map(|claim| claim.task.as_str()).collect();
    registered.sort_unstable();
    registered.dedup();
    for task in registered {
        for scenario in scenario_files(&root.join(SCENARIOS_DIRECTORY).join(task)) {
            for claim in scenario_claim_ids(&scenario) {
                if !declared.contains(claim.as_str()) {
                    return Err(RegistryError::ScenarioClaimUndeclared {
                        task: task.to_owned(),
                        scenario,
                        claim,
                    });
                }
            }
        }
    }
    Ok(())
}

/// The ids of every task under `docs/plans/*/tasks/*.md`, read from the `id:`
/// line of each file's frontmatter. A missing `docs/plans/` is an empty set, so
/// a claims file is then named for no task.
fn task_ids(root: &Path) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let plans = root.join(TASKS_GLOB_ROOT);
    let Ok(entries) = std::fs::read_dir(&plans) else {
        return ids;
    };
    for plan in entries.flatten() {
        let Ok(tasks) = std::fs::read_dir(plan.path().join("tasks")) else {
            continue;
        };
        for task in tasks.flatten() {
            let path = task.path();
            if path.extension().is_some_and(|extension| extension == "md")
                && let Some(id) = frontmatter_id(&path)
            {
                ids.insert(id);
            }
        }
    }
    ids
}

/// The `id:` of a task file's frontmatter, unquoted, if it has one.
fn frontmatter_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("id:"))
        .map(|value| value.trim().trim_matches('"').to_owned())
}

/// Every `*.toml` file directly under a directory, sorted; an empty list when
/// the directory is absent.
///
/// # Errors
///
/// [`RegistryError::Read`] when the directory exists but cannot be read.
fn claim_files(directory: &Path) -> Result<Vec<PathBuf>, RegistryError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RegistryError::Read {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    files.sort();
    Ok(files)
}

/// Every `*.toml` scenario directly under a task's scenario directory, sorted;
/// an empty list when the directory is absent.
fn scenario_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    files.sort();
    files
}

/// The claim ids a scenario file names, or none when it cannot be read as one —
/// scenario format is the scenario harness's to judge, not the registry's.
fn scenario_claim_ids(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<ScenarioClaims>(&text).ok())
        .map(|scenario| scenario.claims)
        .unwrap_or_default()
}

/// A path's file stem as an owned string, lossily.
fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Validate deferred display records without interpreting their content as proof.
///
/// # Errors
/// Returns `DisplayRecord` for paths outside `docs/notes/`, non-Markdown paths,
/// traversal components or records that are not existing files.
fn check_display_records(root: &Path, claims: &[Claim]) -> Result<(), RegistryError> {
    for claim in claims {
        let Proof::Display { record, .. } = &claim.proof else {
            continue;
        };
        let path = Path::new(record);
        if !path.starts_with("docs/notes")
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || path.extension().and_then(|extension| extension.to_str()) != Some("md")
            || !root.join(path).is_file()
        {
            return Err(RegistryError::DisplayRecord {
                task: claim.task.clone(),
                id: claim.id.clone(),
                reason: format!(
                    "`{record}` must be an existing Markdown file under docs/notes with only normal relative path components"
                ),
            });
        }
    }
    Ok(())
}
