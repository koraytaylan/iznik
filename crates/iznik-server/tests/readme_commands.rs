//! Every `iznik-server` entry point, asked what it takes, and matched against
//! the documents that name it.
//!
//! A README that names a command no binary answers to has drifted from the
//! thing it describes, and nothing but a test notices — the document still
//! reads well. So this asks the binary itself which flags it routes, holds
//! the documents to naming every one of them and to naming nothing else, and
//! then runs each with `--help` under a deadline.
//!
//! `--help` and not the work: `--daemon` would start one, and `--stdio` would
//! start one and then talk to it. Answering the question instead of doing the
//! work is the property being asserted as much as it is the way to assert it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};

/// How long one `--help` may take. Generous for a cold binary on a loaded
/// machine, and still a bound.
const HELP_DEADLINE: Duration = Duration::from_secs(30);

/// The flag every entry point must answer.
const HELP_FLAG: &str = "--help";

/// The documents that name this binary's commands, relative to the workspace
/// root.
const DOCUMENTS: &[&str] = &["README.md", "crates/iznik-server/README.md"];

/// What a named command looks like in prose: the program, then the flag,
/// inside a code span or a fenced block.
const PROGRAM: &str = "iznik-server --";

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The workspace root: two directories above this crate's manifest.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Runs the binary with `arguments` and gives back what it said on standard
/// output. A non-zero exit is a failure, which is the whole assertion for
/// `--help`.
///
/// # Errors
///
/// When the binary cannot be started, exits non-zero, or passes its deadline.
fn asked(root: &Path, arguments: &[&str]) -> Result<String, Failed> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iznik-server"));
    command.current_dir(root).args(arguments);
    let answered = process::run(command, Deadline(HELP_DEADLINE), Output::Capture)
        .map_err(|source| format!("iznik-server {}: {source}", arguments.join(" ")))?;
    Ok(String::from_utf8_lossy(&answered.stdout).into_owned())
}

/// The names inside the angle brackets of a usage line, which is how both of
/// this workspace's dispatchers say what they route.
fn routed(usage: &str) -> Vec<String> {
    let Some(open) = usage.find('<') else {
        return Vec::new();
    };
    let Some(close) = usage.find('>') else {
        return Vec::new();
    };
    usage
        .get(open.saturating_add(1)..close)
        .unwrap_or_default()
        .split('|')
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Whether the match at `at` begins a command reference rather than merely
/// mentioning the program's name.
///
/// `cargo nextest run --package iznik-server --test regression_baseline` names
/// this crate, not this command, and the flag that follows is nextest's. A
/// reference starts a code span, starts a line, or follows `cargo `.
fn starts_a_command(text: &str, at: usize) -> bool {
    let before = text.get(..at).unwrap_or_default();
    let before = before.strip_suffix("cargo ").unwrap_or(before);
    before
        .chars()
        .next_back()
        .is_none_or(|letter| letter == '`' || letter == '\n')
}

/// Every entry point of `iznik-server` that the documents name, in the order
/// they first name it.
///
/// # Errors
///
/// When a document cannot be read.
fn named(root: &Path) -> Result<Vec<String>, Failed> {
    let mut found: Vec<String> = Vec::new();
    for document in DOCUMENTS {
        let text = std::fs::read_to_string(root.join(document))?;
        for (at, _matched) in text.match_indices(PROGRAM) {
            if !starts_a_command(&text, at) {
                continue;
            }
            let word: String = text
                .get(at.saturating_add(PROGRAM.len())..)
                .unwrap_or_default()
                .chars()
                .take_while(|letter| letter.is_ascii_lowercase() || *letter == '-')
                .collect();
            let flag = format!("--{word}");
            if !word.is_empty() && !found.contains(&flag) {
                found.push(flag);
            }
        }
    }
    Ok(found)
}

/// # Panics
///
/// When the documents name a command the binary does not route, when the
/// binary routes one no document names, or when any of them does not answer
/// `--help` with success and a word about itself.
#[test]
fn every_named_command_answers_for_itself() {
    let case = || -> Result<(), Failed> {
        let root = workspace_root();
        let routes = routed(&asked(&root, &[HELP_FLAG])?);
        assert!(
            routes.len() > 1,
            "the usage line routes {routes:?}, which means the reading is wrong"
        );
        let documented = named(&root)?;
        let unnamed: Vec<&String> = routes
            .iter()
            .filter(|route| !documented.contains(route))
            .collect();
        assert!(unnamed.is_empty(), "no document names {unnamed:?}");
        let invented: Vec<&String> = documented
            .iter()
            .filter(|command| !routes.contains(command))
            .collect();
        assert!(
            invented.is_empty(),
            "the documents name {invented:?}, which iznik-server does not route"
        );
        for route in &routes {
            let said = asked(&root, &[route, HELP_FLAG])?;
            assert!(
                said.contains(route),
                "iznik-server {route} {HELP_FLAG} says nothing about itself: {said:?}"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
