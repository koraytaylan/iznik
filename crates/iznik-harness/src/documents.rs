//! Holding a binary and the documents that name its commands to each other.
//!
//! A README that names a command no binary answers to still reads well, so
//! nothing but a test notices. This asks the binary itself which commands it
//! routes — every dispatcher in this workspace says so on one usage line —
//! holds the documents to naming every one of them and inventing none, and
//! runs each with `--help` under a deadline.
//!
//! One implementation, three binaries. Three copies of a rule about prose is
//! three chances for one of them to drift into being a different rule.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::process::{self, Deadline, Output, ProcessError};

/// How long one `--help` may take. Generous for a cold binary on a loaded
/// machine, and still a bound.
pub const HELP_DEADLINE: Duration = Duration::from_secs(30);

/// The flag every named command must answer.
pub const HELP_FLAG: &str = "--help";

/// What a binary and its documents are asked to agree about.
#[derive(Clone, Debug)]
pub struct Commands {
    /// The binary to ask.
    pub binary: PathBuf,
    /// The directory the documents are named relative to.
    pub root: PathBuf,
    /// The documents that may name this binary's commands.
    pub documents: Vec<PathBuf>,
    /// What a named command looks like in prose: the program's name and the
    /// space after it, as it appears before a subcommand.
    pub program: String,
}

/// Why a binary and its documents do not agree.
#[derive(Debug)]
pub enum CommandsError {
    /// A document could not be read.
    Read {
        /// The document.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The binary could not be asked.
    Asked {
        /// What was asked.
        arguments: String,
        /// What went wrong.
        source: ProcessError,
    },
    /// The usage line named nothing, so the reading is wrong.
    RoutesNothing,
    /// The binary routes commands no document names.
    Unnamed {
        /// Which.
        commands: Vec<String>,
    },
    /// The documents name commands the binary does not route.
    Invented {
        /// Which.
        commands: Vec<String>,
    },
    /// A command answered `--help` without saying anything about itself.
    Silent {
        /// Which.
        command: String,
        /// What it said instead.
        said: String,
    },
}

impl core::fmt::Display for CommandsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CommandsError::Read { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
            CommandsError::Asked { arguments, source } => {
                write!(formatter, "{arguments}: {source}")
            }
            CommandsError::RoutesNothing => {
                write!(
                    formatter,
                    "the usage line routes nothing, so it was misread"
                )
            }
            CommandsError::Unnamed { commands } => {
                write!(formatter, "no document names {commands:?}")
            }
            CommandsError::Invented { commands } => write!(
                formatter,
                "the documents name {commands:?}, which the binary does not route"
            ),
            CommandsError::Silent { command, said } => write!(
                formatter,
                "`{command} {HELP_FLAG}` says nothing about itself: {said:?}"
            ),
        }
    }
}

impl core::error::Error for CommandsError {}

/// The names inside the angle brackets of a usage line, which is how every
/// dispatcher in this workspace says what it routes.
#[must_use]
pub fn routed(usage: &str) -> Vec<String> {
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
/// Prose runs through these files — "cargo xtask runs the five gates" names no
/// command called `runs`, and `cargo nextest run --package iznik-server
/// --test regression_baseline` names this crate rather than this command — so
/// a reference must start a code span, start a line, or follow `cargo `.
#[must_use]
pub fn starts_a_command(text: &str, at: usize) -> bool {
    let before = text.get(..at).unwrap_or_default();
    let before = before.strip_suffix("cargo ").unwrap_or(before);
    before
        .chars()
        .next_back()
        .is_none_or(|letter| letter == '`' || letter == '\n')
}

impl Commands {
    /// Every command the documents name, in the order they first name it.
    ///
    /// A command is the run of lowercase letters and dashes after a mention of
    /// the program that begins a reference; a leading `--` is kept, so a
    /// binary whose commands are flags is read the same way as one whose
    /// commands are words.
    ///
    /// # Errors
    ///
    /// [`CommandsError::Read`] when a document cannot be read.
    pub fn named(&self) -> Result<Vec<String>, CommandsError> {
        let mut found: Vec<String> = Vec::new();
        for document in &self.documents {
            let path = self.root.join(document);
            let text = std::fs::read_to_string(&path).map_err(|source| CommandsError::Read {
                path: path.clone(),
                source,
            })?;
            for (at, _matched) in text.match_indices(&self.program) {
                if !starts_a_command(&text, at) {
                    continue;
                }
                let word: String = text
                    .get(at.saturating_add(self.program.len())..)
                    .unwrap_or_default()
                    .chars()
                    .take_while(|letter| letter.is_ascii_lowercase() || *letter == '-')
                    .collect();
                if !word.is_empty() && !found.contains(&word) {
                    found.push(word);
                }
            }
        }
        Ok(found)
    }

    /// Runs the binary with `arguments` and gives back what it said on
    /// standard output. A non-zero exit is a failure, which is the whole
    /// assertion for `--help`.
    ///
    /// Standard input is closed, so a subcommand that read it instead of
    /// answering waits on nothing rather than on whoever ran the test.
    ///
    /// # Errors
    ///
    /// [`CommandsError::Asked`] when the binary cannot be started, exits
    /// non-zero, or passes its deadline.
    pub fn ask(&self, arguments: &[&str]) -> Result<String, CommandsError> {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(&self.root)
            .args(arguments)
            .stdin(Stdio::null());
        let answered =
            process::run(command, Deadline(HELP_DEADLINE), Output::Capture).map_err(|source| {
                CommandsError::Asked {
                    arguments: arguments.join(" "),
                    source,
                }
            })?;
        Ok(String::from_utf8_lossy(&answered.stdout).into_owned())
    }

    /// Holds the binary and the documents to each other, and every command to
    /// answering `--help`.
    ///
    /// # Errors
    ///
    /// [`CommandsError`] naming what does not agree.
    pub fn agree(&self) -> Result<Vec<String>, CommandsError> {
        let routes = routed(&self.ask(&[HELP_FLAG])?);
        if routes.is_empty() {
            return Err(CommandsError::RoutesNothing);
        }
        let documented = self.named()?;
        let unnamed: Vec<String> = routes
            .iter()
            .filter(|route| !documented.contains(route))
            .cloned()
            .collect();
        if !unnamed.is_empty() {
            return Err(CommandsError::Unnamed { commands: unnamed });
        }
        let invented: Vec<String> = documented
            .iter()
            .filter(|command| !routes.contains(command))
            .cloned()
            .collect();
        if !invented.is_empty() {
            return Err(CommandsError::Invented { commands: invented });
        }
        for route in &routes {
            let said = self.ask(&[route, HELP_FLAG])?;
            if !said.contains(route) {
                return Err(CommandsError::Silent {
                    command: route.clone(),
                    said,
                });
            }
        }
        Ok(routes)
    }
}

/// The workspace root, `steps` directories above `manifest`.
///
/// Every crate's tests need it and every one of them is a fixed number of
/// directories down from it.
#[must_use]
pub fn workspace_root(manifest: &Path, steps: usize) -> PathBuf {
    manifest
        .ancestors()
        .nth(steps)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}
