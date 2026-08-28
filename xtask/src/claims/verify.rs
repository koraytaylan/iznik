//! Building the nextest filterset from the selected proofs, running them under
//! the `claims` profile, and reading the `JUnit` report back into a verdict per
//! claim.
//!
//! The work splits into pure pieces so the gate's own tests need no container:
//! [`plan`] turns claims into the nextest invocations that prove them,
//! [`command_line`] is the exact argument list of one invocation, and
//! [`interpret_report`] reads a `JUnit` report against a set of claims. [`verify`]
//! is the orchestration that runs the invocations and reads what nextest wrote.
//! A claim whose platform this machine cannot satisfy is deferred, never run;
//! a claim under a non-default cargo profile is run in its own invocation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output, ProcessError};
use iznik_harness::staging::{self, STAGED_VARIABLE, STAGING_DEADLINE, StagingError};

use crate::claims::registry::{self, Claim, Proof, RegistryError};
use crate::claims::selection::{self, Selection, SelectionError};

/// The package that holds every scenario, as a nextest test.
const REGRESSION_PACKAGE: &str = "iznik-regression";

/// The `package::binary` a scenario's test case carries as its class.
const REGRESSION_BINARY: &str = "iznik-regression::regression_scenarios";

/// The `JUnit` report nextest writes under the `claims` profile. Nextest keeps
/// its store under the workspace's own `target/`, not under `CARGO_TARGET_DIR`
/// (verified against the pinned nextest), so this is relative to the repository
/// root, exactly as the architecture states.
const REPORT_PATH: &str = "target/nextest/claims/claims.xml";

/// The nextest profile the proofs run under, whose `JUnit` output this reads.
const CLAIMS_PROFILE: &str = "claims";

/// The program every invocation runs.
const PROGRAM: &str = "cargo";

/// Nextest's exit code when tests ran and some failed — a failed proof, not a
/// failure to run the proofs.
const NEXTEST_TEST_FAILURE: i32 = 100;

/// A test proof's three parts: package, binary, and the test name, which keeps
/// any `::` of its own.
const TEST_PARTS: usize = 3;

/// The verdict on one claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Its proof ran and passed.
    Proven,
    /// Its proof ran and failed.
    Failed {
        /// What failed.
        detail: String,
    },
    /// Its proof did not appear in the report — an unproven claim, never a
    /// proven one.
    Missing,
    /// Its proof could not run here and was not attempted.
    Deferred {
        /// Why it was deferred.
        reason: String,
    },
}

/// The verdict on one claim, with what it claims.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The task that declares it.
    pub task: String,
    /// The claim's id.
    pub id: String,
    /// The claim's statement.
    pub statement: String,
    /// The verdict.
    pub status: Status,
}

/// Every claim's verdict.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// One outcome per claim considered.
    pub outcomes: Vec<Outcome>,
}

impl Report {
    /// Whether every claim is proven or deferred — the report a gate passes on.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| matches!(outcome.status, Status::Proven | Status::Deferred { .. }))
    }
}

/// One nextest run: a filterset over the packages the proofs name, under a
/// cargo profile when the proofs ask for one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    /// The cargo build profile, when the proofs need a non-default one.
    pub profile: Option<String>,
    /// The `-E` filterset: the proofs' atoms, unioned.
    pub filter: String,
    /// The packages the proofs name, each passed as `--package`.
    pub packages: Vec<String>,
}

/// Why a verification could not be carried out — not why a claim failed, which
/// is a [`Status`], but why the run itself could not reach a verdict.
#[derive(Debug)]
pub enum VerifyError {
    /// The registry could not be loaded.
    Registry(RegistryError),
    /// The selection could not be made.
    Selection(SelectionError),
    /// The binaries could not be staged for the scenarios.
    Staging(StagingError),
    /// Nextest could not be run.
    Nextest {
        /// What went wrong.
        detail: String,
    },
    /// The report could not be read.
    Report {
        /// What went wrong.
        detail: String,
    },
}

impl Display for VerifyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Registry(source) => write!(formatter, "the registry: {source}"),
            VerifyError::Selection(source) => write!(formatter, "the selection: {source}"),
            VerifyError::Staging(source) => write!(formatter, "staging: {source}"),
            VerifyError::Nextest { detail } => write!(formatter, "nextest: {detail}"),
            VerifyError::Report { detail } => write!(formatter, "the report: {detail}"),
        }
    }
}

impl std::error::Error for VerifyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VerifyError::Registry(source) => Some(source),
            VerifyError::Selection(source) => Some(source),
            VerifyError::Staging(source) => Some(source),
            VerifyError::Nextest { .. } | VerifyError::Report { .. } => None,
        }
    }
}

/// Verifies the claims a selection resolves to and returns their verdicts.
///
/// # Errors
///
/// [`VerifyError`] when the registry cannot be loaded, the selection cannot be
/// made, the binaries cannot be staged, nextest cannot be run, or its report
/// cannot be read.
pub fn verify(
    root: &Path,
    selection: &Selection,
    deadline: Duration,
) -> Result<Report, VerifyError> {
    let registry = registry::load(root).map_err(VerifyError::Registry)?;
    let tasks = selection::select(root, selection).map_err(VerifyError::Selection)?;
    let selected: Vec<&Claim> = registry
        .claims()
        .iter()
        .filter(|claim| tasks.iter().any(|task| task == &claim.task))
        .collect();
    let runnable: Vec<&Claim> = selected
        .iter()
        .copied()
        .filter(|claim| is_deferred(claim).is_none())
        .collect();
    let cases = if runnable.is_empty() {
        Vec::new()
    } else {
        run_all(root, &runnable, deadline)?
    };
    Ok(Report {
        outcomes: interpret(&selected, &cases),
    })
}

/// The nextest invocations that prove a set of runnable claims: one for the
/// default profile, and one for each non-default cargo profile the claims name.
#[must_use]
pub fn plan(claims: &[&Claim]) -> Vec<Invocation> {
    let mut groups: BTreeMap<Option<String>, Grouping> = BTreeMap::new();
    for claim in claims {
        let (atom, package) = proof_atom(claim);
        let grouping = groups.entry(claim.profile.clone()).or_default();
        grouping.atoms.push(atom);
        grouping.packages.insert(package);
    }
    groups
        .into_iter()
        .map(|(profile, grouping)| Invocation {
            profile,
            filter: grouping.atoms.join(" | "),
            packages: grouping.packages.into_iter().collect(),
        })
        .collect()
}

/// The argument list of one invocation, after the program name.
#[must_use]
pub fn command_line(invocation: &Invocation) -> Vec<String> {
    let mut arguments = vec![
        "nextest".to_owned(),
        "run".to_owned(),
        "--locked".to_owned(),
        "--run-ignored".to_owned(),
        "all".to_owned(),
        "--profile".to_owned(),
        CLAIMS_PROFILE.to_owned(),
        "-E".to_owned(),
        invocation.filter.clone(),
    ];
    if let Some(profile) = &invocation.profile {
        arguments.push("--cargo-profile".to_owned());
        arguments.push(profile.clone());
    }
    for package in &invocation.packages {
        arguments.push("--package".to_owned());
        arguments.push(package.clone());
    }
    arguments
}

/// Reads a `JUnit` report against a set of claims and returns their verdicts —
/// the pure half of [`verify`], for a captured report.
#[must_use]
pub fn interpret_report(claims: &[&Claim], report: &str) -> Vec<Outcome> {
    interpret(claims, &parse_report(report))
}

/// The atoms and packages accumulated for one cargo profile.
#[derive(Default)]
struct Grouping {
    /// One filterset atom per claim in the group.
    atoms: Vec<String>,
    /// Every package the group's proofs name.
    packages: BTreeSet<String>,
}

/// One test case as the `JUnit` report records it.
#[derive(Clone, Debug)]
struct TestCase {
    /// The `package::binary` the case belongs to.
    classname: String,
    /// The test's name within its binary.
    name: String,
    /// Whether it passed.
    passed: bool,
}

/// The filterset atom that runs a claim's proof, and the package it is in.
fn proof_atom(claim: &Claim) -> (String, String) {
    match &claim.proof {
        Proof::Scenario { name } => (
            format!("test(=scenario::{}::{})", claim.task, name),
            REGRESSION_PACKAGE.to_owned(),
        ),
        Proof::Test { name, .. } => {
            let (package, binary, test) = split_test(name);
            (
                format!("package({package}) & binary({binary}) & test(={test})"),
                package,
            )
        }
    }
}

/// A test proof's `package::binary::test` split into its three parts, the test
/// name keeping any `::` of its own.
fn split_test(name: &str) -> (String, String, String) {
    let mut parts = name.splitn(TEST_PARTS, "::");
    let package = parts.next().unwrap_or_default().to_owned();
    let binary = parts.next().unwrap_or_default().to_owned();
    let test = parts.next().unwrap_or_default().to_owned();
    (package, binary, test)
}

/// Runs every invocation and returns the test cases from all their reports.
///
/// # Errors
///
/// [`VerifyError`] when an invocation cannot be run or its report read.
fn run_all(
    root: &Path,
    runnable: &[&Claim],
    deadline: Duration,
) -> Result<Vec<TestCase>, VerifyError> {
    // Stage the binaries once and hand every scenario the staged directory, so
    // the parallel scenario processes never each build and contend on the cargo
    // lock. Only scenarios need it; a run of only test proofs stages nothing.
    let staged = if runnable
        .iter()
        .any(|claim| matches!(claim.proof, Proof::Scenario { .. }))
    {
        Some(staging::stage(Deadline(STAGING_DEADLINE)).map_err(VerifyError::Staging)?)
    } else {
        None
    };
    let mut cases = Vec::new();
    for invocation in plan(runnable) {
        cases.extend(run_one(root, &invocation, deadline, staged.as_deref())?);
    }
    Ok(cases)
}

/// Runs one invocation and returns its report's test cases. The report is
/// removed first, so its absence afterward is a run that produced no results.
///
/// # Errors
///
/// [`VerifyError::Nextest`] when nextest cannot run or fails without a report,
/// and [`VerifyError::Report`] when the report cannot be read.
fn run_one(
    root: &Path,
    invocation: &Invocation,
    deadline: Duration,
    staged: Option<&Path>,
) -> Result<Vec<TestCase>, VerifyError> {
    let report = root.join(REPORT_PATH);
    let _removed = std::fs::remove_file(&report);
    let mut command = Command::new(PROGRAM);
    command.current_dir(root).args(command_line(invocation));
    if let Some(staged) = staged {
        command.env(STAGED_VARIABLE, staged);
    }
    match process::run(command, Deadline(deadline), Output::Inherit) {
        Ok(_completed) => {}
        Err(ProcessError::Failed { status, .. }) if status.code() == Some(NEXTEST_TEST_FAILURE) => {
        }
        Err(ProcessError::Failed {
            program,
            status,
            stderr_tail,
        }) => {
            if !report.is_file() {
                return Err(VerifyError::Nextest {
                    detail: format!("{program} exited {status}: {stderr_tail}"),
                });
            }
        }
        Err(error) => {
            return Err(VerifyError::Nextest {
                detail: error.to_string(),
            });
        }
    }
    let xml = std::fs::read_to_string(&report).map_err(|source| VerifyError::Report {
        detail: format!("{}: {source}", report.display()),
    })?;
    Ok(parse_report(&xml))
}

/// The verdict on each claim, given the test cases that ran.
fn interpret(claims: &[&Claim], cases: &[TestCase]) -> Vec<Outcome> {
    claims
        .iter()
        .map(|claim| Outcome {
            task: claim.task.clone(),
            id: claim.id.clone(),
            statement: claim.statement.clone(),
            status: status_of(claim, cases),
        })
        .collect()
}

/// One claim's status: deferred when its platform is not this one, else the
/// verdict of the case that proves it, or missing when none did.
fn status_of(claim: &Claim, cases: &[TestCase]) -> Status {
    if let Some(reason) = is_deferred(claim) {
        return Status::Deferred { reason };
    }
    let (classname, name) = expected_case(claim);
    match cases
        .iter()
        .find(|case| case.classname == classname && case.name == name)
    {
        None => Status::Missing,
        Some(case) if case.passed => Status::Proven,
        Some(case) => Status::Failed {
            detail: format!("{} failed", case.name),
        },
    }
}

/// The `(classname, name)` a claim's proof appears under in the report.
fn expected_case(claim: &Claim) -> (String, String) {
    match &claim.proof {
        Proof::Scenario { name } => (
            REGRESSION_BINARY.to_owned(),
            format!("scenario::{}::{}", claim.task, name),
        ),
        Proof::Test { name, .. } => {
            let (package, binary, test) = split_test(name);
            (format!("{package}::{binary}"), test)
        }
    }
}

/// The names one platform is known by: what a person writes in a claims file,
/// and what `std::env::consts::OS` calls the same machine.
///
/// A claim that says `darwin` means the platform Rust calls `macos`. Without
/// this the two would never be the same thing, and a proof deferred here
/// would be deferred on the one machine that can run it too — which is a
/// claim nothing will ever establish, reported as though it were merely
/// waiting.
const PLATFORM_ALIASES: &[(&str, &str)] = &[("darwin", "macos")];

/// Whether a platform, named as a claims file names it, is the one running.
#[must_use]
pub fn is_this_platform(platform: &str) -> bool {
    let running = std::env::consts::OS;
    platform == running
        || PLATFORM_ALIASES
            .iter()
            .any(|(written, called)| *written == platform && *called == running)
}

/// Why a claim is deferred, when its platform is not the one running.
fn is_deferred(claim: &Claim) -> Option<String> {
    let platform = claim.platform.as_ref()?;
    if is_this_platform(platform) {
        None
    } else {
        Some(format!(
            "needs platform `{platform}`, but this is `{}`",
            std::env::consts::OS
        ))
    }
}

/// Every test case in a `JUnit` report. A self-closing `<testcase/>` passed; one
/// with a `<failure>` or `<error>` child failed.
fn parse_report(xml: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    for chunk in xml.split("<testcase").skip(1) {
        let Some((tag, after)) = chunk.split_once('>') else {
            continue;
        };
        let (Some(name), Some(classname)) = (attribute(tag, "name"), attribute(tag, "classname"))
        else {
            continue;
        };
        let passed = if tag.trim_end().ends_with('/') {
            true
        } else {
            let content = after.split("</testcase>").next().unwrap_or_default();
            !(content.contains("<failure") || content.contains("<error"))
        };
        cases.push(TestCase {
            classname,
            name,
            passed,
        });
    }
    cases
}

/// The value of a `key="value"` attribute in an element's open tag, decoded.
/// The leading space keeps `name` from matching inside `classname`.
fn attribute(tag: &str, key: &str) -> Option<String> {
    let needle = format!(" {key}=\"");
    let after = tag.split_once(&needle)?.1;
    let value = after.split_once('"')?.0;
    Some(decode_entities(value))
}

/// A string with the five XML entities decoded, `&amp;` last so a decoded
/// ampersand is not decoded again.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
