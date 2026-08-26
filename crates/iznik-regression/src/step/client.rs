//! The `client` step: the client emulator driving a session.
//!
//! A function stub until `integration-harness of plan 0004` fills it: the step dispatcher is complete
//! from the day it is written, so this kind answers [`StepError::Unsupported`]
//! rather than being a missing arm or a hang.

use std::time::Duration;

use crate::step::{Context, Outcome, StepError};

/// The `client` step, unsupported until its plan lands.
///
/// # Errors
///
/// Always [`StepError::Unsupported`].
pub fn execute(
    _context: &Context,
    _body: &toml::Value,
    _timeout: Duration,
) -> Result<Outcome, StepError> {
    Err(StepError::Unsupported {
        kind: "client".to_owned(),
    })
}
