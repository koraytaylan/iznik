//! Every scenario is a nextest test. This `harness = false` binary
//! enumerates every scenario file under `regression/scenarios/` at startup
//! and registers one ignored test per file, named
//! `scenario::<task-id>::<name>`, so a scenario added to the tree becomes a
//! test with no code change; each runs through the one runner and is held to
//! the overhead ceiling.

use std::io::Write;
use std::path::{Path, PathBuf};

use iznik_harness::runner::{self, RunnerError, SCENARIO_OVERHEAD_CEILING};
use iznik_harness::scenario;
use libtest_mimic::{Arguments, Failed, Trial};

/// The repository root: two directories above this crate's manifest.
fn repository_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

/// Every scenario file under `regression/scenarios/`, as (task id, name,
/// path), sorted so the list is stable.
fn scenario_files() -> Vec<(String, String, PathBuf)> {
    let mut found = Vec::new();
    let root = repository_root().join("regression/scenarios");
    let Ok(tasks) = std::fs::read_dir(&root) else {
        return found;
    };
    for task in tasks.flatten() {
        let task_id = task.file_name().to_string_lossy().into_owned();
        let Ok(scenarios) = std::fs::read_dir(task.path()) else {
            continue;
        };
        for scenario in scenarios.flatten() {
            let path = scenario.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                found.push((task_id.clone(), name, path));
            }
        }
    }
    found.sort();
    found
}

/// Runs one scenario and judges it: most must meet their assertions and stay
/// under the overhead ceiling; `budget` must run out of budget, and
/// `timed-out` must report its step killed.
///
/// # Errors
///
/// [`Failed`] with the reason the scenario did not do what it should.
fn run_scenario(path: &Path) -> Result<(), Failed> {
    let scenario = scenario::load(path).map_err(|error| error.to_string())?;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let outcome = runner::run(&scenario, directory);
    // The two scenarios that assert a negative outcome are keyed by name, and
    // so are their steps: `budget`/`too-slow` and `timed-out`/`sleeps`.
    match scenario.name.as_str() {
        "budget" => match outcome {
            Err(RunnerError::BudgetExceeded { step, .. }) if step == "too-slow" => Ok(()),
            other => {
                Err(format!("the budget scenario did not run out of budget: {other:?}").into())
            }
        },
        name => {
            let outcome = outcome.map_err(|error| error.to_string())?;
            let _printed = writeln!(
                std::io::stdout(),
                "{name}: overhead {:?} (ceiling {SCENARIO_OVERHEAD_CEILING:?})",
                outcome.overhead
            );
            if outcome.overhead >= SCENARIO_OVERHEAD_CEILING {
                return Err(format!("overhead {:?} is over the ceiling", outcome.overhead).into());
            }
            if name == "timed-out" {
                let sleeps = outcome
                    .record("sleeps")
                    .ok_or("the timed-out scenario has no `sleeps` record")?;
                if !sleeps.timed_out {
                    return Err(format!("the `sleeps` step was not timed out: {sleeps:?}").into());
                }
            }
            Ok(())
        }
    }
}

/// Enumerates the scenarios into ignored trials and runs the libtest harness.
fn main() {
    let arguments = Arguments::from_args();
    let trials = scenario_files()
        .into_iter()
        .map(|(task_id, name, path)| {
            Trial::test(format!("scenario::{task_id}::{name}"), move || {
                run_scenario(&path)
            })
            .with_ignored_flag(true)
        })
        .collect();
    libtest_mimic::run(&arguments, trials).exit();
}
