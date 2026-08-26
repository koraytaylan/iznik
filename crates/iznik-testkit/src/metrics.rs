//! Resident memory and CPU time of a process from `/proc`: the one
//! implementation behind every memory ceiling, every "memory does not grow"
//! assertion and every "costs no CPU" assertion in the workspace.

use core::fmt::{self, Display, Formatter};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::unistd::{SysconfVar, sysconf};

/// The field of `/proc/<pid>/statm` holding resident pages.
const RESIDENT_PAGES_FIELD: usize = 1;

/// The field of `/proc/<pid>/stat` after the command's closing parenthesis
/// holding the user-mode ticks: `utime`, the fourteenth field of the line.
const USER_TICKS_FIELD: usize = 11;

/// The field after `utime` holding the kernel-mode ticks: `stime`.
const SYSTEM_TICKS_FIELD: usize = 12;

/// Nanoseconds in a second.
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

/// Why a metric could not be read.
#[derive(Debug)]
pub enum MetricsError {
    /// The `/proc` file could not be read: an unknown process id lands here.
    Read {
        /// The process asked about.
        process_id: u32,
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The `/proc` file did not hold the field expected.
    Parse {
        /// The process asked about.
        process_id: u32,
        /// The file.
        path: PathBuf,
        /// What was wrong.
        detail: String,
    },
    /// The system would not say its page size or clock tick.
    Sysconf {
        /// The variable asked for.
        variable: &'static str,
        /// What went wrong.
        source: nix::Error,
    },
}

impl Display for MetricsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            MetricsError::Read {
                process_id,
                path,
                source,
            } => write!(
                formatter,
                "process {process_id}: {} could not be read: {source}",
                path.display()
            ),
            MetricsError::Parse {
                process_id,
                path,
                detail,
            } => write!(
                formatter,
                "process {process_id}: {} is not as expected: {detail}",
                path.display()
            ),
            MetricsError::Sysconf { variable, source } => {
                write!(formatter, "the system would not say {variable}: {source}")
            }
        }
    }
}

impl std::error::Error for MetricsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MetricsError::Read { source, .. } => Some(source),
            MetricsError::Sysconf { source, .. } => Some(source),
            MetricsError::Parse { .. } => None,
        }
    }
}

/// A system constant, as an unsigned number.
///
/// # Errors
///
/// [`MetricsError::Sysconf`] when the system will not say, or says nothing.
fn system_constant(variable: SysconfVar, name: &'static str) -> Result<u64, MetricsError> {
    let value = sysconf(variable)
        .map_err(|source| MetricsError::Sysconf {
            variable: name,
            source,
        })?
        .ok_or(MetricsError::Sysconf {
            variable: name,
            source: nix::Error::ENOTSUP,
        })?;
    u64::try_from(value).map_err(|_negative| MetricsError::Sysconf {
        variable: name,
        source: nix::Error::ERANGE,
    })
}

/// The text of a process's `/proc` file.
///
/// # Errors
///
/// [`MetricsError::Read`] when it cannot be read, naming the process.
fn proc_file(process_id: u32, file: &str) -> Result<(PathBuf, String), MetricsError> {
    let path = PathBuf::from(format!("/proc/{process_id}/{file}"));
    let text = std::fs::read_to_string(&path).map_err(|source| MetricsError::Read {
        process_id,
        path: path.clone(),
        source,
    })?;
    Ok((path, text))
}

/// A whitespace-separated field of a `/proc` line, as a number.
///
/// # Errors
///
/// [`MetricsError::Parse`] when the field is absent or not a number.
fn field(process_id: u32, path: &Path, text: &str, index: usize) -> Result<u64, MetricsError> {
    let parse = MetricsError::Parse {
        process_id,
        path: path.to_path_buf(),
        detail: format!("field {index} is missing or not a number"),
    };
    text.split_whitespace()
        .nth(index)
        .and_then(|word| word.parse().ok())
        .ok_or(parse)
}

/// The resident memory of a process in bytes, from `/proc/<pid>/statm`.
///
/// # Errors
///
/// [`MetricsError::Read`] for an unknown process, [`MetricsError::Parse`]
/// for a file not as expected, [`MetricsError::Sysconf`] when the page size
/// is unknown.
pub fn resident_memory(process_id: u32) -> Result<u64, MetricsError> {
    let (path, text) = proc_file(process_id, "statm")?;
    let pages = field(process_id, &path, &text, RESIDENT_PAGES_FIELD)?;
    let page_size = system_constant(SysconfVar::PAGE_SIZE, "the page size")?;
    Ok(pages.saturating_mul(page_size))
}

/// The CPU time a process has used in user and kernel mode, from
/// `/proc/<pid>/stat`.
///
/// # Errors
///
/// [`MetricsError::Read`] for an unknown process, [`MetricsError::Parse`]
/// for a file not as expected, [`MetricsError::Sysconf`] when the clock
/// tick is unknown.
pub fn cpu_time(process_id: u32) -> Result<Duration, MetricsError> {
    let (path, text) = proc_file(process_id, "stat")?;
    let (_command, after_command) = text.rsplit_once(')').ok_or(MetricsError::Parse {
        process_id,
        path: path.clone(),
        detail: "no closing parenthesis after the command".to_owned(),
    })?;
    let user = field(process_id, &path, after_command, USER_TICKS_FIELD)?;
    let system = field(process_id, &path, after_command, SYSTEM_TICKS_FIELD)?;
    let ticks = user.saturating_add(system);
    let ticks_per_second = system_constant(SysconfVar::CLK_TCK, "the clock tick")?;
    let nanoseconds = ticks
        .saturating_mul(NANOSECONDS_PER_SECOND)
        .checked_div(ticks_per_second)
        .ok_or(MetricsError::Sysconf {
            variable: "the clock tick",
            source: nix::Error::ERANGE,
        })?;
    Ok(Duration::from_nanos(nanoseconds))
}
