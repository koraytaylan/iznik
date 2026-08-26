//! `xtask regression images`, `stage` and `reap`: the container images, the
//! staging directory, and the removal of every labelled container. `images`
//! builds the two images that are missing, tagged by their Containerfiles'
//! content, and prints both references; `stage` and `reap` are filled by
//! task `regression-fixture` of plan 0001.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use iznik_harness::images::{IMAGE_BUILD_DEADLINE, ensure_images};
use iznik_harness::process::Deadline;

use crate::USAGE_EXIT_CODE;

/// The subcommand's forms.
const FORMS: &str = "images | stage | reap";

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let form = arguments
        .get(1)
        .map(|argument| argument.to_string_lossy().into_owned());
    match form.as_deref() {
        Some("images") => images(),
        Some(pending @ ("stage" | "reap")) => {
            let _written = writeln!(
                std::io::stderr(),
                "{pending}: not implemented until task regression-fixture"
            );
            ExitCode::from(USAGE_EXIT_CODE)
        }
        _ => {
            let _written = writeln!(std::io::stderr(), "usage: xtask regression <{FORMS}>");
            ExitCode::from(USAGE_EXIT_CODE)
        }
    }
}

/// Ensures both images and prints their references.
fn images() -> ExitCode {
    match ensure_images(Deadline(IMAGE_BUILD_DEADLINE)) {
        Ok(images) => {
            let _written = writeln!(
                std::io::stdout(),
                "host: {}\nengine: {}",
                images.host,
                images.engine
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _written = writeln!(std::io::stderr(), "regression images: {error}");
            ExitCode::FAILURE
        }
    }
}
