//! `xtask doctor`: every prerequisite of `CONTRIBUTING.md` section 1 with a
//! probe and an install hint, reported by name when missing. `xtask check`
//! runs it first, so a missing tool is reported by name instead of surfacing
//! as a failed gate.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output, ProcessError};

/// What this subcommand takes, which is nothing.
const USAGE: &str = "usage: xtask doctor";

/// How long a probe may take: thirty seconds — `podman info` on a cold
/// machine is the slowest, and takes seconds.
const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// The file that pins the toolchain, relative to the repository root.
const TOOLCHAIN_FILE: &str = "rust-toolchain.toml";

/// One prerequisite: what it is, how it is probed, what the probe must print,
/// and how to install it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prerequisite {
    /// The name a person knows it by.
    pub name: String,
    /// The program the probe runs.
    pub program: &'static str,
    /// The arguments the probe hands it.
    pub arguments: &'static [&'static str],
    /// Text the probe's standard output must contain, when running is not
    /// enough.
    pub expected_output: Option<String>,
    /// How to install it.
    pub install: String,
}

impl Prerequisite {
    /// The probe as a command line, for messages.
    fn probe_line(&self) -> String {
        std::iter::once(self.program)
            .chain(self.arguments.iter().copied())
            .collect::<Vec<&str>>()
            .join(" ")
    }
}

/// A prerequisite that is not there: its name, why, and how to install it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    /// The prerequisite's name.
    pub name: String,
    /// What the probe found.
    pub reason: String,
    /// How to install it.
    pub install: String,
}

impl Display for Missing {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {}\n  install: {}",
            self.name, self.reason, self.install
        )
    }
}

/// Why the doctor could not examine the machine at all.
#[derive(Debug)]
pub enum DoctorError {
    /// The pinned toolchain could not be read from `rust-toolchain.toml`.
    Toolchain {
        /// The file.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },
}

impl Display for DoctorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            DoctorError::Toolchain { path, detail } => {
                write!(formatter, "{}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for DoctorError {}

/// The channel `rust-toolchain.toml` pins, which the toolchain probe must
/// report.
///
/// # Errors
///
/// [`DoctorError::Toolchain`] when the file cannot be read or names no
/// channel.
pub fn pinned_channel(root: &Path) -> Result<String, DoctorError> {
    let path = root.join(TOOLCHAIN_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| DoctorError::Toolchain {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|error| DoctorError::Toolchain {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    table
        .get("toolchain")
        .and_then(|toolchain| toolchain.get("channel"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DoctorError::Toolchain {
            path,
            detail: "no toolchain.channel".to_owned(),
        })
}

/// The prerequisites, in the order `CONTRIBUTING.md` section 1 lists them;
/// the toolchain's expected version is read from `rust-toolchain.toml`.
///
/// # Errors
///
/// [`DoctorError::Toolchain`] when the pinned channel cannot be read.
pub fn prerequisites(root: &Path) -> Result<Vec<Prerequisite>, DoctorError> {
    let channel = pinned_channel(root)?;
    Ok(vec![
        Prerequisite {
            name: format!("rust toolchain {channel}"),
            program: "cargo",
            arguments: &["--version"],
            expected_output: Some(channel.clone()),
            install: format!(
                "rustup toolchain install {channel}; rustup does it on first use inside this repository"
            ),
        },
        Prerequisite {
            name: "cargo-nextest".to_owned(),
            program: "cargo",
            arguments: &["nextest", "--version"],
            expected_output: None,
            install: "cargo install cargo-nextest --locked".to_owned(),
        },
        Prerequisite {
            name: "podman with the netavark network backend".to_owned(),
            program: "podman",
            arguments: &["info", "--format", "{{.Host.NetworkBackend}}"],
            expected_output: Some("netavark".to_owned()),
            install: "install podman and netavark from your distribution (Debian and Ubuntu: apt install podman netavark); if podman reports another backend, set network_backend = \"netavark\" in containers.conf".to_owned(),
        },
        Prerequisite {
            name: "zig".to_owned(),
            program: "zig",
            arguments: &["version"],
            expected_output: None,
            install: "install zig from https://ziglang.org/download/ and put it on PATH".to_owned(),
        },
        Prerequisite {
            name: "x86_64-linux-musl-gcc".to_owned(),
            program: "x86_64-linux-musl-gcc",
            arguments: &["--version"],
            expected_output: None,
            install: "install a musl cross-compiler under that name: a musl.cc toolchain, or a shim over `zig cc -target x86_64-linux-musl`".to_owned(),
        },
        Prerequisite {
            name: "aarch64-linux-musl-gcc".to_owned(),
            program: "aarch64-linux-musl-gcc",
            arguments: &["--version"],
            expected_output: None,
            install: "install a musl cross-compiler under that name: a musl.cc toolchain, or a shim over `zig cc -target aarch64-linux-musl`".to_owned(),
        },
        Prerequisite {
            name: "git".to_owned(),
            program: "git",
            arguments: &["--version"],
            expected_output: None,
            install: "install git from your distribution (Debian and Ubuntu: apt install git)".to_owned(),
        },
    ])
}

/// What a probe said on its standard error, ready to append to a reason;
/// nothing when it said nothing.
fn said(stderr_tail: &str) -> String {
    let trimmed = stderr_tail.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("; it said: {trimmed}")
    }
}

/// What a failed probe means to a person, with what the probe said.
fn describe(prerequisite: &Prerequisite, error: &ProcessError) -> String {
    match error {
        ProcessError::Spawn { program, source } if source.kind() == ErrorKind::NotFound => {
            format!("`{program}` is not on PATH")
        }
        ProcessError::Spawn { program, source } => {
            format!("`{program}` could not be started: {source}")
        }
        ProcessError::TimedOut {
            deadline,
            stderr_tail,
            ..
        } => format!(
            "`{}` did not finish within {deadline:?}{}",
            prerequisite.probe_line(),
            said(stderr_tail)
        ),
        ProcessError::Failed {
            status,
            stderr_tail,
            ..
        } => format!(
            "`{}` failed with {status}{}",
            prerequisite.probe_line(),
            said(stderr_tail)
        ),
        ProcessError::Wait { source, .. } => format!(
            "`{}` could not be watched to its end: {source}",
            prerequisite.probe_line()
        ),
    }
}

/// Runs a prerequisite's probe and judges its output.
///
/// # Errors
///
/// Why the prerequisite is missing, in a person's words.
fn probe(prerequisite: &Prerequisite) -> Result<(), String> {
    let mut command = Command::new(prerequisite.program);
    command.args(prerequisite.arguments);
    let completed = process::run(command, Deadline(PROBE_DEADLINE), Output::Capture)
        .map_err(|error| describe(prerequisite, &error))?;
    let Some(expected) = &prerequisite.expected_output else {
        return Ok(());
    };
    let output = String::from_utf8_lossy(&completed.stdout);
    if output.contains(expected.as_str()) {
        return Ok(());
    }
    Err(format!(
        "`{}` reports {} rather than {expected}",
        prerequisite.probe_line(),
        output.trim()
    ))
}

/// Examines every prerequisite, in order: present, or missing with the
/// reason.
///
/// # Errors
///
/// [`DoctorError::Toolchain`] when the pinned channel cannot be read.
pub fn examine(root: &Path) -> Result<Vec<Result<Prerequisite, Missing>>, DoctorError> {
    Ok(prerequisites(root)?
        .into_iter()
        .map(|prerequisite| match probe(&prerequisite) {
            Ok(()) => Ok(prerequisite),
            Err(reason) => Err(Missing {
                name: prerequisite.name,
                reason,
                install: prerequisite.install,
            }),
        })
        .collect())
}

/// Every prerequisite that is missing, in order.
///
/// # Errors
///
/// [`DoctorError::Toolchain`] when the pinned channel cannot be read.
pub fn missing(root: &Path) -> Result<Vec<Missing>, DoctorError> {
    Ok(examine(root)?.into_iter().filter_map(Result::err).collect())
}

/// The entry point of `xtask doctor`: one `ok:` line per present prerequisite
/// on standard output, one `missing:` line with its install hint per absent
/// one on standard error, and a failure status when anything is missing.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    if arguments.len() != 1 {
        writeln!(io::stderr(), "{USAGE}").unwrap_or_default();
        return ExitCode::from(crate::USAGE_EXIT_CODE);
    }
    let examined = match examine(&crate::repository_root()) {
        Ok(examined) => examined,
        Err(error) => {
            writeln!(io::stderr(), "doctor: {error}").unwrap_or_default();
            return ExitCode::FAILURE;
        }
    };
    let mut anything_missing = false;
    for outcome in examined {
        match outcome {
            Ok(prerequisite) => {
                writeln!(io::stdout(), "ok: {}", prerequisite.name).unwrap_or_default();
            }
            Err(missing) => {
                anything_missing = true;
                writeln!(io::stderr(), "missing: {missing}").unwrap_or_default();
            }
        }
    }
    if anything_missing {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
