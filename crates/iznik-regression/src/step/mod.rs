//! The step dispatcher: one step read as TOML on standard input, executed in
//! its own process group under its deadline, reported as one NDJSON record.
//! The dispatcher is complete on the day it is written — every kind has a
//! module — so the eight kinds later plans fill are function stubs that
//! answer `Unsupported`, never a hang and never a missing arm.

pub mod bootstrap;
pub mod channel;
pub mod client;
pub mod manager;
pub mod pane;
pub mod probe;
pub mod run;
pub mod transport;
pub mod upload;

use core::fmt::{self, Display, Formatter};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use iznik_harness::fixture::MOUNT_POINT;
use iznik_harness::report::Record;

/// The exit code the record carries for a step whose kind is not yet
/// executable: the usage code, so it is told from a real run.
const UNSUPPORTED_EXIT: i32 = 2;

/// One step's outcome, before it is wrapped in a [`Record`].
#[derive(Debug)]
pub struct Outcome {
    /// The exit code the command reported, or `None` on a timeout.
    pub exit: Option<i32>,
    /// Whether the deadline killed it.
    pub timed_out: bool,
    /// How long it took.
    pub duration: Duration,
    /// The standard output, lossy UTF-8.
    pub stdout: String,
    /// The standard error, lossy UTF-8.
    pub stderr: String,
}

/// Why a step could not be executed. A step that runs and fails is not an
/// error here — its non-zero exit is in the record; only the harness itself
/// failing is.
#[derive(Debug)]
pub enum StepError {
    /// The step's kind has no executor yet.
    Unsupported {
        /// The kind, and the plan that fills it.
        kind: String,
    },
    /// The step read from standard input is not a step.
    Malformed {
        /// What is wrong.
        detail: String,
    },
    /// Standard input could not be read.
    Input {
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The command could not be run at all.
    Execution {
        /// What the process runner said.
        source: iznik_harness::process::ProcessError,
    },
}

impl Display for StepError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            StepError::Unsupported { kind } => {
                write!(
                    formatter,
                    "the `{kind}` step is not implemented in this plan"
                )
            }
            StepError::Malformed { detail } => write!(formatter, "the step is malformed: {detail}"),
            StepError::Input { source } => {
                write!(formatter, "the step could not be read: {source}")
            }
            StepError::Execution { source } => {
                write!(formatter, "the step could not be run: {source}")
            }
        }
    }
}

impl std::error::Error for StepError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StepError::Input { source } => Some(source),
            StepError::Execution { source } => Some(source),
            StepError::Unsupported { .. } | StepError::Malformed { .. } => None,
        }
    }
}

/// The driver's paths: the staged directory mounted in the container and the
/// binaries and distribution tree under it. Every step is handed this, so a
/// kind a later plan fills finds the server, the client or a shipped artifact
/// the one way — the dispatcher's call site is written once and never grows an
/// argument as kinds are filled.
#[derive(Clone, Debug)]
pub struct Context {
    /// The staged directory, mounted read-only in the container.
    staged: PathBuf,
}

impl Context {
    /// The context for a driver in a fixture container, where the staged
    /// directory is mounted at [`MOUNT_POINT`].
    #[must_use]
    pub fn mounted() -> Context {
        Context::new(PathBuf::from(MOUNT_POINT))
    }

    /// The context for a staged directory at `staged`, for a test that puts it
    /// somewhere other than the container's mount.
    #[must_use]
    pub fn new(staged: PathBuf) -> Context {
        Context { staged }
    }

    /// The staged directory itself.
    #[must_use]
    pub fn staged(&self) -> &Path {
        &self.staged
    }

    /// The path of a staged binary, under `bin/`.
    #[must_use]
    pub fn binary(&self, name: &str) -> PathBuf {
        self.staged.join("bin").join(name)
    }

    /// The distribution tree, where the bootstrap looks for a shipped server.
    #[must_use]
    pub fn distribution(&self) -> PathBuf {
        self.staged.join("distribution")
    }
}

/// The plan that fills each kind, for the message an unsupported step
/// carries; `run` is filled here.
fn filling_plan(kind: &str) -> &'static str {
    match kind {
        "pane" => "plan 0002",
        "client" => "plan 0004",
        _ => "plan 0005",
    }
}

/// A malformed-step error with a message.
fn malformed(detail: impl Into<String>) -> StepError {
    StepError::Malformed {
        detail: detail.into(),
    }
}

/// The whole step as a TOML table, read from standard input.
///
/// # Errors
///
/// [`StepError::Input`] when input cannot be read, [`StepError::Malformed`]
/// when it is not a step table.
fn read_step() -> Result<toml::Table, StepError> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|source| StepError::Input { source })?;
    text.parse::<toml::Table>()
        .map_err(|source| malformed(source.to_string()))
}

/// A string field of the step table.
///
/// # Errors
///
/// [`StepError::Malformed`] when it is absent or not a string.
fn string_field(step: &toml::Table, key: &str) -> Result<String, StepError> {
    step.get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("no string `{key}`")))
}

/// Executes the one kind the step names and returns its outcome.
///
/// # Errors
///
/// [`StepError`] when the kind is unsupported or the command cannot run.
fn dispatch(step: &toml::Table, timeout: Duration) -> Result<Outcome, StepError> {
    let kind = iznik_harness::scenario::STEP_KINDS
        .iter()
        .copied()
        .find(|kind| step.contains_key(*kind))
        .ok_or_else(|| malformed("the step has no kind"))?;
    let body = step
        .get(kind)
        .unwrap_or(&toml::Value::Boolean(false))
        .clone();
    // Every kind is called with the same three arguments, so filling a kind a
    // later plan owns is editing its file alone, never this dispatcher.
    let context = Context::mounted();
    match kind {
        "run" => run::execute(&context, &body, timeout),
        "bootstrap" => bootstrap::execute(&context, &body, timeout),
        "channel" => channel::execute(&context, &body, timeout),
        "client" => client::execute(&context, &body, timeout),
        "manager" => manager::execute(&context, &body, timeout),
        "pane" => pane::execute(&context, &body, timeout),
        "probe" => probe::execute(&context, &body, timeout),
        "transport" => transport::execute(&context, &body, timeout),
        "upload" => upload::execute(&context, &body, timeout),
        // `fault` is executed by the runner, never sent to the driver.
        other => Err(StepError::Unsupported {
            kind: other.to_owned(),
        }),
    }
}

/// The record for a step: its outcome, or the unsupported note as a record so
/// the runner always reads exactly one line and never hangs.
fn record_of(scenario: &str, step: &str, outcome: Result<Outcome, StepError>) -> Record {
    match outcome {
        Ok(outcome) => Record {
            scenario: scenario.to_owned(),
            step: step.to_owned(),
            exit: outcome.exit,
            timed_out: outcome.timed_out,
            duration_milliseconds: duration_milliseconds(outcome.duration),
            stdout: outcome.stdout,
            stderr: outcome.stderr,
        },
        Err(StepError::Unsupported { kind }) => Record {
            scenario: scenario.to_owned(),
            step: step.to_owned(),
            exit: Some(UNSUPPORTED_EXIT),
            timed_out: false,
            duration_milliseconds: 0,
            stdout: String::new(),
            stderr: format!("the `{kind}` step is filled by {}", filling_plan(&kind)),
        },
        Err(error) => Record {
            scenario: scenario.to_owned(),
            step: step.to_owned(),
            exit: Some(UNSUPPORTED_EXIT),
            timed_out: false,
            duration_milliseconds: 0,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

/// A duration in whole milliseconds, saturating.
#[must_use]
pub fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The `step` subcommand: read one step, run it, print one NDJSON record.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first; this subcommand takes none, reading the step from
/// standard input.
#[must_use]
pub fn run(_arguments: &[OsString]) -> ExitCode {
    let step = match read_step() {
        Ok(step) => step,
        Err(error) => return fail(&error),
    };
    let scenario = string_field(&step, "scenario").unwrap_or_else(|_error| String::new());
    let id = string_field(&step, "id").unwrap_or_else(|_error| String::new());
    let seconds = step
        .get("timeout_seconds")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0);
    let outcome = dispatch(&step, Duration::from_secs(seconds));
    let record = record_of(&scenario, &id, outcome);
    match record.to_ndjson() {
        Ok(line) => {
            let _written = writeln!(std::io::stdout(), "{line}");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&malformed(error.to_string())),
    }
}

/// The driver's own failure — not a step's — on standard error and the
/// usage code, so the runner sees the harness broke, not a step.
fn fail(error: &StepError) -> ExitCode {
    let _written = writeln!(std::io::stderr(), "iznik-regression step: {error}");
    ExitCode::from(crate::USAGE_EXIT_CODE)
}
