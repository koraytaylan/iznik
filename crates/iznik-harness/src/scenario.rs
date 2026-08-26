//! The scenario format: a scenario is TOML data, so an acceptance criterion
//! is something a reviewer reads without trusting the harness that runs it.
//! Every table denies unknown keys; every step declares its deadline and
//! every scenario its budget; a step's kind is exactly one of a fixed set,
//! and only the envelope — `id`, `container`, `timeout_seconds` — is parsed
//! here, the kind's own table being the executing module's to read.

use core::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;

use crate::report::Record;

/// The most a scenario may budget: anything longer is a soak, not a scenario.
pub const MAXIMUM_SCENARIO_BUDGET_SECONDS: u64 = 300;

/// The kinds a step may be. Only `run` and `fault` are executed by this
/// plan; the rest are envelopes their later plans fill.
pub const STEP_KINDS: &[&str] = &[
    "run",
    "fault",
    "pane",
    "client",
    "transport",
    "channel",
    "probe",
    "upload",
    "bootstrap",
    "manager",
];

/// The envelope keys every step carries, which are not its kind.
const ENVELOPE_KEYS: &[&str] = &["id", "container", "timeout_seconds"];

/// Why a scenario is not a scenario.
#[derive(Debug)]
pub enum ScenarioError {
    /// The file could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The bytes are not the TOML a scenario is.
    Toml {
        /// What the parser said.
        source: toml::de::Error,
    },
    /// The TOML parses but is not a well-formed scenario.
    Malformed {
        /// What is wrong, named.
        detail: String,
    },
}

impl Display for ScenarioError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ScenarioError::Read { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
            ScenarioError::Toml { source } => {
                write!(formatter, "the scenario is not TOML: {source}")
            }
            ScenarioError::Malformed { detail } => {
                write!(formatter, "the scenario is malformed: {detail}")
            }
        }
    }
}

impl std::error::Error for ScenarioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScenarioError::Read { source, .. } => Some(source),
            ScenarioError::Toml { source } => Some(source),
            ScenarioError::Malformed { .. } => None,
        }
    }
}

/// A malformed-scenario error with a message.
fn malformed(detail: impl Into<String>) -> ScenarioError {
    ScenarioError::Malformed {
        detail: detail.into(),
    }
}

/// What a scenario sets up before its steps.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// How many hosts the fixture starts.
    pub hosts: usize,
    /// Files copied into the driver's home before the steps run, each a path
    /// relative to the scenario's directory.
    #[serde(default)]
    pub files: Vec<PathBuf>,
}

/// One assertion about a step's record.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// The step the assertions are about.
    pub step: String,
    /// The exit code the step must report.
    #[serde(default)]
    pub exit: Option<i32>,
    /// The exact standard output.
    #[serde(default)]
    pub stdout_equals: Option<String>,
    /// A substring of the standard output.
    #[serde(default)]
    pub stdout_contains: Option<String>,
    /// A regular expression the standard output must match.
    #[serde(default)]
    pub stdout_matches: Option<String>,
    /// The exact standard error.
    #[serde(default)]
    pub stderr_equals: Option<String>,
    /// A substring of the standard error.
    #[serde(default)]
    pub stderr_contains: Option<String>,
    /// A regular expression the standard error must match.
    #[serde(default)]
    pub stderr_matches: Option<String>,
    /// The seconds the step must have taken less than.
    #[serde(default)]
    pub duration_under_seconds: Option<u64>,
}

/// The fixed-shape part of a scenario, before the steps are validated.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    /// The scenario's name; the file's stem repeats it.
    name: String,
    /// The claims this scenario proves, by id.
    #[serde(default)]
    claims: Vec<String>,
    /// The container the scenario is driven from and files are copied into.
    driver: String,
    /// The whole scenario's budget in seconds.
    budget_seconds: u64,
    /// Whether the scenario needs the machine to itself.
    #[serde(default)]
    exclusive: bool,
    /// The setup.
    setup: Setup,
    /// The steps, each still a raw table until its envelope and kind are read.
    #[serde(default)]
    steps: Vec<toml::Table>,
    /// The assertions.
    #[serde(default)]
    expect: Vec<Expect>,
}

/// One step: its envelope and its kind, the kind's body left as read.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The step's id, unique within the scenario.
    pub id: String,
    /// The container the step runs in.
    pub container: String,
    /// The step's deadline.
    pub timeout: Duration,
    /// The kind's name, one of [`STEP_KINDS`].
    pub kind: String,
    /// The kind's value, for the module that executes the kind.
    pub body: toml::Value,
}

/// A whole scenario, validated.
#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    /// The name.
    pub name: String,
    /// The claims it proves.
    pub claims: Vec<String>,
    /// The container it is driven from.
    pub driver: String,
    /// The whole scenario's budget.
    pub budget: Duration,
    /// Whether it needs the machine to itself.
    pub exclusive: bool,
    /// The setup.
    pub setup: Setup,
    /// The steps, in order.
    pub steps: Vec<Step>,
    /// The assertions.
    pub expect: Vec<Expect>,
}

/// A string field of a raw step table.
///
/// # Errors
///
/// [`ScenarioError::Malformed`] when the field is absent or not a string.
fn string_field(table: &toml::Table, key: &str, step: &str) -> Result<String, ScenarioError> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("step `{step}` has no string `{key}`")))
}

/// The one kind key of a step table, with its value.
///
/// # Errors
///
/// [`ScenarioError::Malformed`] when a key is neither the envelope nor a
/// kind, when no kind is present, or when more than one is.
fn kind_of(table: &toml::Table, step: &str) -> Result<(String, toml::Value), ScenarioError> {
    let mut found: Option<String> = None;
    for key in table.keys() {
        if ENVELOPE_KEYS.contains(&key.as_str()) {
            continue;
        }
        if !STEP_KINDS.contains(&key.as_str()) {
            return Err(malformed(format!(
                "step `{step}` has an unknown key `{key}`"
            )));
        }
        if let Some(first) = &found {
            return Err(malformed(format!(
                "step `{step}` has two kinds, `{first}` and `{key}`; a step is exactly one"
            )));
        }
        found = Some(key.clone());
    }
    let kind = found.ok_or_else(|| {
        malformed(format!(
            "step `{step}` has no kind; it must be one of {STEP_KINDS:?}"
        ))
    })?;
    let body = table
        .get(&kind)
        .cloned()
        .unwrap_or(toml::Value::Boolean(false));
    Ok((kind, body))
}

/// One raw step table validated into a [`Step`].
///
/// # Errors
///
/// [`ScenarioError::Malformed`] when the envelope or the kind is wrong.
fn validate_step(table: &toml::Table) -> Result<Step, ScenarioError> {
    let id = string_field(table, "id", "<unnamed>")?;
    let container = string_field(table, "container", &id)?;
    let seconds = table
        .get("timeout_seconds")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| malformed(format!("step `{id}` has no `timeout_seconds`")))?;
    let (kind, body) = kind_of(table, &id)?;
    Ok(Step {
        id,
        container,
        timeout: Duration::from_secs(seconds),
        kind,
        body,
    })
}

/// The scenario a string of TOML holds.
///
/// # Errors
///
/// [`ScenarioError::Toml`] when it is not TOML, [`ScenarioError::Malformed`]
/// when a budget is over the maximum, a step is not well-formed, two steps
/// share an id, or an assertion names a step that does not exist.
pub fn parse(text: &str) -> Result<Scenario, ScenarioError> {
    let envelope: Envelope =
        toml::from_str(text).map_err(|source| ScenarioError::Toml { source })?;
    if envelope.budget_seconds > MAXIMUM_SCENARIO_BUDGET_SECONDS {
        return Err(malformed(format!(
            "the budget of {} seconds is over the maximum of {MAXIMUM_SCENARIO_BUDGET_SECONDS}",
            envelope.budget_seconds
        )));
    }
    let mut steps = Vec::new();
    let mut ids = Vec::new();
    for table in &envelope.steps {
        let step = validate_step(table)?;
        if ids.contains(&step.id) {
            return Err(malformed(format!("two steps share the id `{}`", step.id)));
        }
        ids.push(step.id.clone());
        steps.push(step);
    }
    for expectation in &envelope.expect {
        if !ids.contains(&expectation.step) {
            return Err(malformed(format!(
                "an assertion names step `{}`, which no step has",
                expectation.step
            )));
        }
    }
    Ok(Scenario {
        name: envelope.name,
        claims: envelope.claims,
        driver: envelope.driver,
        budget: Duration::from_secs(envelope.budget_seconds),
        exclusive: envelope.exclusive,
        setup: envelope.setup,
        steps,
        expect: envelope.expect,
    })
}

/// How many milliseconds a second is, for the duration assertion.
const MILLISECONDS_PER_SECOND: u64 = 1000;

/// Whether a record satisfies every assertion of an expectation; the first
/// that fails is the error.
///
/// # Errors
///
/// A sentence naming the assertion that failed and what the record held, or
/// a `stdout_matches`/`stderr_matches` pattern that is not a regular
/// expression.
pub fn evaluate(expect: &Expect, record: &Record) -> Result<(), String> {
    if let Some(exit) = expect.exit
        && record.exit != Some(exit)
    {
        return Err(format!(
            "expected exit {exit}, the record has {:?}",
            record.exit
        ));
    }
    check_stream(
        "stdout",
        &record.stdout,
        expect.stdout_equals.as_deref(),
        expect.stdout_contains.as_deref(),
        expect.stdout_matches.as_deref(),
    )?;
    check_stream(
        "stderr",
        &record.stderr,
        expect.stderr_equals.as_deref(),
        expect.stderr_contains.as_deref(),
        expect.stderr_matches.as_deref(),
    )?;
    if let Some(seconds) = expect.duration_under_seconds {
        let bound = seconds.saturating_mul(MILLISECONDS_PER_SECOND);
        if record.duration_milliseconds >= bound {
            return Err(format!(
                "expected under {seconds}s, the record took {}ms",
                record.duration_milliseconds
            ));
        }
    }
    Ok(())
}

/// The equality, containment and match assertions on one named stream.
///
/// # Errors
///
/// A sentence naming the failed assertion, or a pattern that is not a
/// regular expression.
fn check_stream(
    name: &str,
    actual: &str,
    equals: Option<&str>,
    contains: Option<&str>,
    matches: Option<&str>,
) -> Result<(), String> {
    if let Some(want) = equals
        && actual != want
    {
        return Err(format!(
            "expected {name} `{want}`, the record has `{actual}`"
        ));
    }
    if let Some(want) = contains
        && !actual.contains(want)
    {
        return Err(format!(
            "expected {name} to contain `{want}`, the record has `{actual}`"
        ));
    }
    if let Some(pattern) = matches {
        let regex =
            Regex::new(pattern).map_err(|error| format!("`{pattern}` is not a regex: {error}"))?;
        if !regex.is_match(actual) {
            return Err(format!(
                "expected {name} to match `{pattern}`, the record has `{actual}`"
            ));
        }
    }
    Ok(())
}

/// The scenario a file holds.
///
/// # Errors
///
/// [`ScenarioError::Read`] when the file cannot be read, else as [`parse`].
pub fn load(path: &Path) -> Result<Scenario, ScenarioError> {
    let text = std::fs::read_to_string(path).map_err(|source| ScenarioError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}
