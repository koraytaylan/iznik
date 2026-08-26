//! The `channel` step: a pane output channel with flow control.
//!
//! A function stub until `remote-channel of plan 0005` fills it: the step dispatcher is complete
//! from the day it is written, so this kind answers [`StepError::Unsupported`]
//! rather than being a missing arm or a hang.

use std::time::Duration;

use crate::step::{Context, Outcome, StepError};

/// The `channel` step, unsupported until its plan lands.
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
        kind: "channel".to_owned(),
    })
}
