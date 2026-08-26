//! The claims registry: what a task claims about runtime behavior, the proof
//! that establishes each claim, and the gate that runs the proofs.
//!
//! `registry` loads and validates the claims files, `selection` decides which
//! tasks a run covers, and `verify` runs the selected proofs and reads their
//! report. `xtask claims verify` is the fifth gate: with no `--task` it covers
//! the current branch, and it fails when the branch changes code without
//! declaring claims. `xtask claims coverage` covers every task that has a
//! claims file, which is how the trunk — whose branch diff is empty — is proven.

pub mod registry;
pub mod selection;
pub mod verify;

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::claims::selection::Selection;
use crate::claims::verify::{Outcome, Report, Status, VerifyError};

/// A task id: the stem of a claims file, and how a selection names a task.
pub type TaskId = String;

/// How long one nextest invocation of the selected proofs may run before it is
/// a failure, not a wait; a typical run is far under it.
const RUN_DEADLINE: Duration = Duration::from_mins(15);

/// The `claims` subcommand: `verify [--task <id>]… [--root <dir>]` or
/// `coverage [--root <dir>]`.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    match arguments.get(1).and_then(|argument| argument.to_str()) {
        Some("verify") => command(&verify_selection(arguments), arguments),
        Some("coverage") => command(&Selection::Everything, arguments),
        _ => usage(),
    }
}

/// The selection `verify` covers: the named tasks, or the current branch when
/// none are named.
fn verify_selection(arguments: &[OsString]) -> Selection {
    let tasks = task_arguments(arguments);
    if tasks.is_empty() {
        Selection::CurrentBranch
    } else {
        Selection::Tasks(tasks)
    }
}

/// Verifies a selection under the root the arguments name, prints the report,
/// and exits by whether it holds.
fn command(selection: &Selection, arguments: &[OsString]) -> ExitCode {
    let root = root_argument(arguments).unwrap_or_else(crate::repository_root);
    match verify::verify(&root, selection, RUN_DEADLINE) {
        Ok(report) => {
            print_report(&report);
            if report.holds() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => fail(&error),
    }
}

/// Prints one line per claim and a one-line summary.
fn print_report(report: &Report) {
    let _printed = writeln!(std::io::stdout(), "{}", render(report));
}

/// The whole report as text: one line per claim, a failure's or a deferral's
/// reason indented under it, and a summary — or that nothing was selected.
fn render(report: &Report) -> String {
    if report.outcomes.is_empty() {
        return "claims: no claims selected".to_owned();
    }
    let lines: Vec<String> = report.outcomes.iter().map(render_outcome).collect();
    let proven = count(report, |status| matches!(status, Status::Proven));
    let failed = count(report, |status| matches!(status, Status::Failed { .. }));
    let missing = count(report, |status| matches!(status, Status::Missing));
    let deferred = count(report, |status| matches!(status, Status::Deferred { .. }));
    format!(
        "{}\nclaims: {proven} proven, {failed} failed, {missing} missing, {deferred} deferred",
        lines.join("\n")
    )
}

/// One claim's line, with a failure's or deferral's reason indented under it.
fn render_outcome(outcome: &Outcome) -> String {
    let head = format!(
        "{}: {} — {} ({})",
        word(&outcome.status),
        outcome.id,
        outcome.statement,
        outcome.task
    );
    match &outcome.status {
        Status::Failed { detail } => format!("{head}\n    {detail}"),
        Status::Deferred { reason } => format!("{head}\n    {reason}"),
        Status::Proven | Status::Missing => head,
    }
}

/// How many outcomes have a status the predicate accepts.
fn count(report: &Report, predicate: impl Fn(&Status) -> bool) -> usize {
    report
        .outcomes
        .iter()
        .filter(|outcome| predicate(&outcome.status))
        .count()
}

/// The word a status is reported with.
fn word(status: &Status) -> &'static str {
    match status {
        Status::Proven => "proven",
        Status::Failed { .. } => "failed",
        Status::Missing => "missing",
        Status::Deferred { .. } => "deferred",
    }
}

/// Reports a verification that could not be carried out, and the failure code.
fn fail(error: &VerifyError) -> ExitCode {
    let _printed = writeln!(std::io::stderr(), "claims: {error}");
    ExitCode::FAILURE
}

/// Reports an unusable command line and the usage code.
fn usage() -> ExitCode {
    let _printed = writeln!(std::io::stderr(), "claims: expected `verify` or `coverage`");
    ExitCode::from(crate::USAGE_EXIT_CODE)
}

/// Every task id named by a `--task <id>` on the command line, in order.
fn task_arguments(arguments: &[OsString]) -> Vec<TaskId> {
    let mut tasks = Vec::new();
    let mut iterator = arguments.iter();
    while let Some(argument) = iterator.next() {
        if argument.as_os_str() == "--task"
            && let Some(value) = iterator.next()
        {
            tasks.push(value.to_string_lossy().into_owned());
        }
    }
    tasks
}

/// The value following `--root`, when the command line carries one.
fn root_argument(arguments: &[OsString]) -> Option<PathBuf> {
    let mut iterator = arguments.iter();
    iterator
        .find(|argument| argument.as_os_str() == "--root")
        .and_then(|_flag| iterator.next())
        .map(PathBuf::from)
}
