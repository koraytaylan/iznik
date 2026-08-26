//! A fixture: only inert names.
use std::process::ExitCode;
pub fn code() -> ExitCode {
    let identifier = std::process::id();
    ExitCode::SUCCESS
}
