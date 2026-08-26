//! The `pane` step: a pane checked through the VT oracle.
//!
//! A function stub until `fidelity-suite of plan 0002` fills it: the step dispatcher is complete
//! from the day it is written, so this kind answers [`StepError::Unsupported`]
//! rather than being a missing arm or a hang.

use std::time::Duration;

use crate::step::{Context, Outcome, StepError};

/// The `pane` step, unsupported until its plan lands.
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
        kind: "pane".to_owned(),
    })
}
