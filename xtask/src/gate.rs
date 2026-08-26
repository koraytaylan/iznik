//! `xtask check` and `xtask gate <name>`: the five gates in order, each under
//! its deadline, stopping at the first failure with its name.
//!
//! This table is the one `CONTRIBUTING.md` section 2 tabulates;
//! `.makina/config.toml` transcribes it, and `xtask/tests/gate_runner.rs`
//! holds the transcription to this table, so a task that passes locally is a
//! task that lands. A gate's output streams through as it happens — Makina's
//! idle watchdog reads that stream — so a failed gate's output is already on
//! the terminal when its name is reported.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::io::{self, Write};
use std::process::{Command, ExitCode};
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output, ProcessError};

use crate::doctor::{self, DoctorError, Missing};

/// The program every gate runs.
pub const PROGRAM: &str = "cargo";

/// The format gate's deadline: five minutes for `cargo fmt --check`.
const FORMAT_DEADLINE: Duration = Duration::from_mins(5);

/// The lint gate's deadline: fifteen minutes for clippy over every target.
const LINT_DEADLINE: Duration = Duration::from_mins(15);

/// The documentation gate's deadline: ten minutes for rustdoc over the
/// workspace, private items included.
const DOCUMENTATION_DEADLINE: Duration = Duration::from_mins(10);

/// The test gate's deadline: fifteen minutes for every in-process test.
const TEST_DEADLINE: Duration = Duration::from_mins(15);

/// The claims gate's deadline: fifteen minutes for the proofs of the tasks a
/// branch changes; a typical run takes two.
const CLAIMS_DEADLINE: Duration = Duration::from_mins(15);

/// The environment the documentation gate runs under: rustdoc warnings are
/// errors.
const DOCUMENTATION_ENVIRONMENT: &[(&str, &str)] = &[("RUSTDOCFLAGS", "-D warnings")];

/// The five gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// `cargo fmt --all --check`.
    Format,
    /// `cargo clippy` over every target with warnings denied.
    Lint,
    /// `cargo doc` over the workspace with warnings denied.
    Documentation,
    /// `cargo nextest run` over the workspace, the policy tests included.
    Test,
    /// `cargo xtask claims verify`: the proofs of the claims the branch
    /// declares.
    Claims,
}

/// The gates in the order `check` runs them.
pub const GATES: &[Gate] = &[
    Gate::Format,
    Gate::Lint,
    Gate::Documentation,
    Gate::Test,
    Gate::Claims,
];

impl Gate {
    /// The gate's name, as `xtask gate <name>` and `.makina/config.toml`
    /// spell it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Gate::Format => "format",
            Gate::Lint => "lint",
            Gate::Documentation => "documentation",
            Gate::Test => "test",
            Gate::Claims => "claims",
        }
    }

    /// The gate with `name`, if there is one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Gate> {
        GATES.iter().copied().find(|gate| gate.name() == name)
    }

    /// The arguments the gate hands to [`PROGRAM`].
    #[must_use]
    pub fn arguments(self) -> &'static [&'static str] {
        match self {
            Gate::Format => &["fmt", "--all", "--check"],
            Gate::Lint => &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
            Gate::Documentation => &[
                "doc",
                "--workspace",
                "--no-deps",
                "--document-private-items",
                "--locked",
            ],
            Gate::Test => &["nextest", "run", "--workspace", "--locked"],
            Gate::Claims => &["xtask", "claims", "verify"],
        }
    }

    /// The whole command the gate runs: the program first, then its
    /// arguments.
    #[must_use]
    pub fn command(self) -> Vec<&'static str> {
        std::iter::once(PROGRAM)
            .chain(self.arguments().iter().copied())
            .collect()
    }

    /// The environment variables the gate's command runs with, beyond the
    /// inherited environment.
    #[must_use]
    pub fn environment(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Gate::Documentation => DOCUMENTATION_ENVIRONMENT,
            Gate::Format | Gate::Lint | Gate::Test | Gate::Claims => &[],
        }
    }

    /// How long the gate may run before it is ended and reported as hung: a
    /// deadline catches a hang, and is not what a run is allowed to take.
    #[must_use]
    pub fn deadline(self) -> Duration {
        match self {
            Gate::Format => FORMAT_DEADLINE,
            Gate::Lint => LINT_DEADLINE,
            Gate::Documentation => DOCUMENTATION_DEADLINE,
            Gate::Test => TEST_DEADLINE,
            Gate::Claims => CLAIMS_DEADLINE,
        }
    }
}

impl Display for Gate {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Why `check` or a gate did not pass.
#[derive(Debug)]
pub enum GateError {
    /// The doctor could not examine the machine.
    Doctor(DoctorError),
    /// A prerequisite is missing; the gates were not run.
    Prerequisite(Missing),
    /// A gate failed or hung.
    Gate {
        /// The gate.
        gate: Gate,
        /// What its command did.
        source: ProcessError,
    },
}

impl Display for GateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            GateError::Doctor(error) => write!(formatter, "doctor: {error}"),
            GateError::Prerequisite(missing) => {
                write!(formatter, "prerequisite missing: {missing}")
            }
            GateError::Gate { gate, source } => write!(
                formatter,
                "gate {gate} failed running `{}`: {source}; its output is above",
                gate.command().join(" ")
            ),
        }
    }
}

impl std::error::Error for GateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GateError::Doctor(error) => Some(error),
            GateError::Gate { source, .. } => Some(source),
            GateError::Prerequisite(_) => None,
        }
    }
}

/// Runs one gate in the repository root with its output streamed through,
/// and returns how long it took.
///
/// # Errors
///
/// [`GateError::Gate`] when the gate's command fails or exceeds its deadline.
pub fn run_gate(gate: Gate) -> Result<Duration, GateError> {
    let mut command = Command::new(PROGRAM);
    command
        .args(gate.arguments())
        .current_dir(crate::repository_root());
    for (name, value) in gate.environment() {
        command.env(name, value);
    }
    let completed = process::run(command, Deadline(gate.deadline()), Output::Inherit)
        .map_err(|source| GateError::Gate { gate, source })?;
    Ok(completed.elapsed)
}

/// Runs the doctor, then every gate in order, announcing each and printing
/// one line per gate with its elapsed time, and stopping at the first missing
/// prerequisite or failing gate.
///
/// # Errors
///
/// [`GateError::Doctor`] when the machine cannot be examined,
/// [`GateError::Prerequisite`] naming the first missing prerequisite, and
/// [`GateError::Gate`] naming the first gate that failed.
pub fn check() -> Result<(), GateError> {
    let missing = doctor::missing(&crate::repository_root()).map_err(GateError::Doctor)?;
    if let Some(first) = missing.into_iter().next() {
        return Err(GateError::Prerequisite(first));
    }
    for gate in GATES {
        writeln!(io::stdout(), "{gate}: running").unwrap_or_default();
        let elapsed = run_gate(*gate)?;
        report_passed(*gate, elapsed);
    }
    Ok(())
}

/// The line printed for a gate that passed.
fn report_passed(gate: Gate, elapsed: Duration) {
    writeln!(
        io::stdout(),
        "{gate}: passed in {:.1}s",
        elapsed.as_secs_f64()
    )
    .unwrap_or_default();
}

/// The usage line, on standard error, and the usage exit code.
fn usage() -> ExitCode {
    let names: Vec<&str> = GATES.iter().map(|gate| gate.name()).collect();
    writeln!(
        io::stderr(),
        "usage: xtask check | xtask gate <{}>",
        names.join(" | ")
    )
    .unwrap_or_default();
    ExitCode::from(crate::USAGE_EXIT_CODE)
}

/// The entry point of `xtask check` and `xtask gate <name>`.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let outcome = match arguments {
        [subcommand] if subcommand.as_os_str() == "check" => check(),
        [subcommand, name] if subcommand.as_os_str() == "gate" => {
            match name.to_str().and_then(Gate::parse) {
                Some(gate) => run_gate(gate).map(|elapsed| report_passed(gate, elapsed)),
                None => return usage(),
            }
        }
        _ => return usage(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            writeln!(io::stderr(), "{error}").unwrap_or_default();
            ExitCode::FAILURE
        }
    }
}
