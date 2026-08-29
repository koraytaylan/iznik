//! Every `xtask` command the documents name, asked what it takes.
//!
//! A README that names a command no binary answers to has drifted from the
//! thing it describes, and nothing but a test notices. The rule itself lives
//! in `iznik_harness::documents`, one implementation for all three binaries;
//! what is here is which binary, which documents, and what a mention of it
//! looks like in prose.
//!
//! `--help` and not the work: `xtask check` would run the whole gate, and a
//! test that ran the gate would be the gate.
//!
//! `CONTRIBUTING.md` is here as well as the two READMEs because it is where a
//! person is told which command to run, and a rule that names a gate nothing
//! answers to is a rule nobody can follow.

use std::path::{Path, PathBuf};

use iznik_harness::documents::{Commands, workspace_root};

/// # Panics
///
/// When the documents name a command the binary does not route, when the
/// binary routes one no document names, or when any of them does not answer
/// `--help` with success and a word about itself.
#[test]
fn every_named_command_answers_for_itself() {
    let commands = Commands {
        binary: PathBuf::from(env!("CARGO_BIN_EXE_xtask")),
        root: workspace_root(Path::new(env!("CARGO_MANIFEST_DIR")), 1),
        documents: vec![
            PathBuf::from("README.md"),
            PathBuf::from("CONTRIBUTING.md"),
            PathBuf::from("xtask/README.md"),
        ],
        program: "xtask ".to_owned(),
    };
    // `agree` refuses a usage line that routes nothing and every disagreement
    // between the documents and the binary, so reaching here is the assertion.
    let _agreed = commands.agree().unwrap_or_else(|error| panic!("{error}"));
}
