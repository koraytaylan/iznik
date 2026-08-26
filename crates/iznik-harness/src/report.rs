//! The record a step reports: one NDJSON line the driver prints and the
//! runner reads. Standard output and error are UTF-8 with lossy replacement,
//! never raw bytes — binary output goes to files, never into a record — so a
//! record is always a valid line of JSON.

use serde::{Deserialize, Serialize};

/// One step's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The scenario the step belongs to.
    pub scenario: String,
    /// The step's id.
    pub step: String,
    /// The exit code the step's command reported, or `None` when it was
    /// killed at its deadline before it could report one.
    pub exit: Option<i32>,
    /// Whether the step was killed at its deadline.
    pub timed_out: bool,
    /// How long the step took, in milliseconds.
    pub duration_milliseconds: u64,
    /// The step's standard output, lossy UTF-8.
    pub stdout: String,
    /// The step's standard error, lossy UTF-8.
    pub stderr: String,
}

impl Record {
    /// The record as one line of NDJSON, without a trailing newline.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] only if a field cannot serialize, which for
    /// these fields cannot happen.
    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// The record one line of NDJSON holds.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the line is not a record.
    pub fn from_ndjson(line: &str) -> Result<Record, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// The lossy UTF-8 of some bytes: the form a record holds output in.
#[must_use]
pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
