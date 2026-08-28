//! `xtask regression images`, `stage` and `reap`: the container images, the
//! staging directory, and the removal of every labelled container. `images`
//! builds the two images that are missing, tagged by their Containerfiles'
//! content, and prints both references; `stage` builds and lays out the
//! three musl binaries under a content hash and prints the directory; `reap`
//! removes every container and network with the fixture's labels, for a
//! person.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use iznik_harness::fixture::{Reap, reap};
use iznik_harness::images::{IMAGE_BUILD_DEADLINE, ensure_images};
use iznik_harness::process::Deadline;
use iznik_harness::staging::{STAGING_DEADLINE, stage};

use crate::USAGE_EXIT_CODE;

/// The subcommand's forms.
const FORMS: &str = "images | stage | reap";

/// What this subcommand takes: one of its three forms.
fn usage_line() -> String {
    format!("usage: xtask regression <{FORMS}>")
}

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(&usage_line());
    }
    let form = arguments
        .get(1)
        .map(|argument| argument.to_string_lossy().into_owned());
    match form.as_deref() {
        Some("images") => images(),
        Some("stage") => staged(),
        Some("reap") => reaped(),
        _ => {
            let _written = writeln!(std::io::stderr(), "{}", usage_line());
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

/// Stages the binaries and prints the directory.
fn staged() -> ExitCode {
    match stage(Deadline(STAGING_DEADLINE)) {
        Ok(directory) => {
            let _written = writeln!(std::io::stdout(), "{}", directory.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _written = writeln!(std::io::stderr(), "regression stage: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Removes everything with the fixture's labels and prints how many.
fn reaped() -> ExitCode {
    match reap(Reap::Everything) {
        Ok(count) => {
            let _written = writeln!(std::io::stdout(), "removed {count}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _written = writeln!(std::io::stderr(), "regression reap: {error}");
            ExitCode::FAILURE
        }
    }
}
