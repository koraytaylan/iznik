//! The `run` step: a shell command in the container that names it, run in
//! its own process group under its deadline. The real exit code is recovered
//! from a file so a command that exits non-zero still keeps its standard
//! output — the process runner treats a non-zero exit as an error and drops
//! it — and a command killed at the deadline is reported with its partial
//! output and `timed_out`.

use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output, ProcessError};
use iznik_harness::report::lossy;

use crate::step::{Context, Outcome, StepError};

/// Where the wrapper writes the command's real exit code.
const EXIT_FILE: &str = "/tmp/iznik-step.rc";

/// The `run` step. Its body is the command, a string.
///
/// # Errors
///
/// [`StepError::Malformed`] when the body is not a string,
/// [`StepError::Execution`] when the command cannot be spawned or waited for.
pub fn execute(
    _context: &Context,
    body: &toml::Value,
    timeout: Duration,
) -> Result<Outcome, StepError> {
    let command = body.as_str().ok_or_else(|| StepError::Malformed {
        detail: "a `run` step's value is the command, a string".to_owned(),
    })?;
    // The command runs in a subshell so its own `exit` does not skip the
    // capture of its status; the status goes to a file, and the outer shell
    // always succeeds, so the process runner returns the output whatever the
    // command's exit was.
    let wrapped = format!("( {command} )\nstatus=$?\nprintf %s \"$status\" > {EXIT_FILE}\nexit 0");
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(wrapped);
    let _removed = std::fs::remove_file(EXIT_FILE);
    match process::run(shell, Deadline(timeout), Output::Capture) {
        Ok(completed) => Ok(Outcome {
            exit: Some(recorded_exit()),
            timed_out: false,
            duration: completed.elapsed,
            stdout: lossy(&completed.stdout),
            stderr: lossy(&completed.stderr),
        }),
        Err(ProcessError::TimedOut {
            deadline,
            stdout_tail,
            stderr_tail,
            ..
        }) => Ok(Outcome {
            exit: None,
            timed_out: true,
            duration: deadline,
            stdout: stdout_tail,
            stderr: stderr_tail,
        }),
        Err(source) => Err(StepError::Execution { source }),
    }
}

/// The exit code the wrapper wrote, or zero when the file is unreadable —
/// which can only happen if the command left it so, since the wrapper wrote
/// it before the outer shell succeeded.
fn recorded_exit() -> i32 {
    std::fs::read_to_string(EXIT_FILE)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}
