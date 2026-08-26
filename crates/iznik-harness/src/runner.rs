//! The one runner every scenario goes through: stage the binaries, start the
//! fixture, copy the scenario's files into the driver's home, run each step
//! in order — a `fault` through the fixture, every other kind inside the
//! container it names by the driver binary — evaluate the assertions, hold
//! the whole to its budget, and report the overhead beyond the steps.

use core::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::fixture::{Fault, Fixture, FixtureError, FixtureOptions, Process};
use crate::report::Record;
use crate::scenario::{Scenario, Step, evaluate};
use crate::staging::{STAGING_DEADLINE, StagingError, stage};

/// The most a scenario's overhead beyond its steps may be: fixture start,
/// file copy and per-step handoff together.
pub const SCENARIO_OVERHEAD_CEILING: Duration = Duration::from_secs(30);

/// How much longer than a step's own deadline the runner waits for the
/// driver to run it and report, before the wait is the budget's to end.
const STEP_MARGIN: Duration = Duration::from_secs(15);

/// How long a copy or an input write into a container may take.
const HELPER_DEADLINE: Duration = Duration::from_secs(30);

/// The driver binary inside the container, under the staged mount.
const DRIVER: &str = "/iznik/bin/iznik-regression";

/// Where a step's input TOML is written in the container it runs in.
const STEP_INPUT: &str = "/tmp/iznik-step-input.toml";

/// The heredoc delimiter for writing a step's input, chosen not to appear in
/// a step.
const HEREDOC: &str = "IZNIK_STEP_EOF";

/// A scenario's outcome: the record of every step, and the overhead.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// One record per step, in order.
    pub records: Vec<Record>,
    /// The scenario's wall time minus the sum of its steps' durations.
    pub overhead: Duration,
}

impl Outcome {
    /// The record of the step with this id, if the step ran.
    #[must_use]
    pub fn record(&self, step: &str) -> Option<&Record> {
        self.records.iter().find(|record| record.step == step)
    }
}

/// Why a scenario could not be run, or did not meet itself.
#[derive(Debug)]
pub enum RunnerError {
    /// The binaries could not be staged.
    Staging(StagingError),
    /// The fixture could not be started or worked.
    Fixture(FixtureError),
    /// A step's record could not be read from the driver's output.
    Record {
        /// The step.
        step: String,
        /// What was wrong.
        detail: String,
    },
    /// A scenario file names a fault the runner does not know.
    Fault {
        /// The step.
        step: String,
        /// What was wrong.
        detail: String,
    },
    /// The scenario ran past its budget while a step was running.
    BudgetExceeded {
        /// The step that was running.
        step: String,
        /// The budget.
        budget: Duration,
    },
    /// A step's record did not satisfy an assertion.
    Expectation {
        /// One line per failed assertion.
        failures: Vec<String>,
    },
}

impl Display for RunnerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            RunnerError::Staging(source) => write!(formatter, "staging: {source}"),
            RunnerError::Fixture(source) => write!(formatter, "the fixture: {source}"),
            RunnerError::Record { step, detail } => {
                write!(formatter, "step `{step}` reported no record: {detail}")
            }
            RunnerError::Fault { step, detail } => {
                write!(formatter, "step `{step}` names an unknown fault: {detail}")
            }
            RunnerError::BudgetExceeded { step, budget } => write!(
                formatter,
                "the budget of {budget:?} ran out while step `{step}` was running"
            ),
            RunnerError::Expectation { failures } => {
                write!(formatter, "assertions failed: {}", failures.join("; "))
            }
        }
    }
}

impl std::error::Error for RunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunnerError::Staging(source) => Some(source),
            RunnerError::Fixture(source) => Some(source),
            _ => None,
        }
    }
}

impl From<StagingError> for RunnerError {
    fn from(source: StagingError) -> RunnerError {
        RunnerError::Staging(source)
    }
}

impl From<FixtureError> for RunnerError {
    fn from(source: FixtureError) -> RunnerError {
        RunnerError::Fixture(source)
    }
}

/// The fault a `fault` step names, resolved against its container.
///
/// # Errors
///
/// [`RunnerError::Fault`] when the value is not a fault the runner knows.
fn fault_of(step: &Step) -> Result<Fault, RunnerError> {
    let unknown = |detail: String| RunnerError::Fault {
        step: step.id.clone(),
        detail,
    };
    let container = step.container.clone();
    if let Some(action) = step.body.as_str() {
        return match action {
            "disconnect-network" => Ok(Fault::DisconnectNetwork { container }),
            "reconnect-network" => Ok(Fault::ReconnectNetwork { container }),
            other => Err(unknown(format!("`{other}`"))),
        };
    }
    let table = step
        .body
        .as_table()
        .ok_or_else(|| unknown("a fault is a string or a table".to_owned()))?;
    let action = table
        .get("action")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| unknown("a fault table has a string `action`".to_owned()))?;
    let file = table
        .get("file")
        .and_then(toml::Value::as_str)
        .map(PathBuf::from);
    let process = || {
        file.clone()
            .map(Process::IdFile)
            .ok_or_else(|| unknown("a process fault has a `file`".to_owned()))
    };
    match action {
        "kill-process" => Ok(Fault::KillProcess {
            container,
            process: process()?,
        }),
        "pause-process" => Ok(Fault::PauseProcess {
            container,
            process: process()?,
        }),
        "resume-process" => Ok(Fault::ResumeProcess {
            container,
            process: process()?,
        }),
        other => Err(unknown(format!("`{other}`"))),
    }
}

/// The input TOML the driver reads for a step, with the scenario named.
///
/// # Errors
///
/// [`RunnerError::Record`] when the step cannot be serialized.
fn step_input(scenario: &str, step: &Step) -> Result<String, RunnerError> {
    let mut table = toml::Table::new();
    table.insert(
        "scenario".to_owned(),
        toml::Value::String(scenario.to_owned()),
    );
    table.insert("id".to_owned(), toml::Value::String(step.id.clone()));
    table.insert(
        "container".to_owned(),
        toml::Value::String(step.container.clone()),
    );
    let seconds = i64::try_from(step.timeout.as_secs()).unwrap_or(i64::MAX);
    table.insert("timeout_seconds".to_owned(), toml::Value::Integer(seconds));
    table.insert(step.kind.clone(), step.body.clone());
    toml::to_string(&table).map_err(|error| RunnerError::Record {
        step: step.id.clone(),
        detail: error.to_string(),
    })
}

/// Runs one non-fault step inside its container by the driver and returns
/// its record.
///
/// # Errors
///
/// [`RunnerError`] when the input cannot be written, the driver cannot be
/// run within the deadline, or its output is not one record.
fn run_in_container(
    fixture: &Fixture,
    scenario: &str,
    step: &Step,
    deadline: Duration,
) -> Result<Record, RunnerError> {
    let input = step_input(scenario, step)?;
    let write = format!("cat > {STEP_INPUT} <<'{HEREDOC}'\n{input}\n{HEREDOC}\n");
    fixture.exec(&step.container, &write, HELPER_DEADLINE)?;
    let run = format!("{DRIVER} step < {STEP_INPUT}");
    let completed = fixture.exec(&step.container, &run, deadline)?;
    let output = String::from_utf8_lossy(&completed.stdout);
    let line = output.lines().next().ok_or_else(|| RunnerError::Record {
        step: step.id.clone(),
        detail: "the driver printed nothing".to_owned(),
    })?;
    Record::from_ndjson(line).map_err(|error| RunnerError::Record {
        step: step.id.clone(),
        detail: error.to_string(),
    })
}

/// A fault step's record: the fault is injected and the step is instant.
fn fault_record(scenario: &str, step: &str, elapsed: Duration) -> Record {
    Record {
        scenario: scenario.to_owned(),
        step: step.to_owned(),
        exit: Some(0),
        timed_out: false,
        duration_milliseconds: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        stdout: String::new(),
        stderr: String::new(),
    }
}

/// Copies the scenario's files into the driver container's home.
///
/// # Errors
///
/// [`RunnerError`] when a file cannot be read or written.
fn copy_files(
    fixture: &Fixture,
    directory: &Path,
    driver: &str,
    files: &[PathBuf],
) -> Result<(), RunnerError> {
    for file in files {
        let source = directory.join(file);
        let bytes = std::fs::read(&source).map_err(|error| RunnerError::Record {
            step: "setup".to_owned(),
            detail: format!("{}: {error}", source.display()),
        })?;
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let write = format!("cat > ~/{name} <<'{HEREDOC}'\n{text}\n{HEREDOC}\n");
        fixture.exec(driver, &write, HELPER_DEADLINE)?;
    }
    Ok(())
}

/// Runs a scenario and reports its records and overhead. The fixture is torn
/// down when this returns, whatever the outcome.
///
/// # Errors
///
/// [`RunnerError`] when staging or the fixture fails, a step's record cannot
/// be read, the budget runs out, or an assertion is not met.
pub fn run(scenario: &Scenario, directory: &Path) -> Result<Outcome, RunnerError> {
    let staged = stage(crate::process::Deadline(STAGING_DEADLINE))?;
    let started = Instant::now();
    let fixture = Fixture::start(FixtureOptions::new(scenario.setup.hosts, staged))?;
    copy_files(&fixture, directory, &scenario.driver, &scenario.setup.files)?;
    let mut records = Vec::new();
    let mut step_total = Duration::ZERO;
    for step in &scenario.steps {
        let remaining = scenario.budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(RunnerError::BudgetExceeded {
                step: step.id.clone(),
                budget: scenario.budget,
            });
        }
        if step.kind == "fault" {
            let fault_started = Instant::now();
            fixture.fault(&fault_of(step)?)?;
            records.push(fault_record(
                &scenario.name,
                &step.id,
                fault_started.elapsed(),
            ));
            continue;
        }
        let by_step = step.timeout.saturating_add(STEP_MARGIN);
        let deadline = by_step.min(remaining);
        match run_in_container(&fixture, &scenario.name, step, deadline) {
            Ok(record) => {
                step_total =
                    step_total.saturating_add(Duration::from_millis(record.duration_milliseconds));
                records.push(record);
            }
            Err(error) => {
                if remaining <= by_step && started.elapsed() >= scenario.budget {
                    return Err(RunnerError::BudgetExceeded {
                        step: step.id.clone(),
                        budget: scenario.budget,
                    });
                }
                return Err(error);
            }
        }
    }
    // Tear the fixture down before measuring, so the overhead the architecture
    // defines — wall minus step durations, teardown included — is bounded.
    drop(fixture);
    let overhead = started.elapsed().saturating_sub(step_total);
    let outcome = Outcome { records, overhead };
    check_expectations(scenario, &outcome)?;
    Ok(outcome)
}

/// Every assertion of the scenario against the records.
///
/// # Errors
///
/// [`RunnerError::Expectation`] with one line per failed assertion, or per
/// assertion naming a step that produced no record.
fn check_expectations(scenario: &Scenario, outcome: &Outcome) -> Result<(), RunnerError> {
    let mut failures = Vec::new();
    for expectation in &scenario.expect {
        match outcome.record(&expectation.step) {
            Some(record) => {
                if let Err(failure) = evaluate(expectation, record) {
                    failures.push(format!("step `{}`: {failure}", expectation.step));
                }
            }
            None => failures.push(format!("step `{}` produced no record", expectation.step)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RunnerError::Expectation { failures })
    }
}
