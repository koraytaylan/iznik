//! Resident memory and CPU time of a process, asked of `ps`: the one
//! implementation behind every memory ceiling, every "memory does not grow"
//! assertion and every "costs no CPU" assertion in the workspace.
//!
//! The kernel exposes these facts differently on each platform this runs on —
//! a `/proc` file on Linux, `libproc` on macOS — but `ps` prints the same two
//! numbers on both, and asking it costs one process per reading. Nothing here
//! parses a platform's own files, so a reading is the same reading wherever a
//! test runs.

use core::fmt::{self, Display, Formatter};
use std::io;
use std::process::Command;
use std::time::Duration;

/// The program asked for a process's numbers.
const PS: &str = "ps";

/// The flag that names the process to ask about.
const PROCESS_FLAG: &str = "-p";

/// The flag introducing the field asked for.
const FORMAT_FLAG: &str = "-o";

/// The format asking for resident memory alone, in kibibytes, unheaded and
/// unpadded so that what comes back is the number.
const RESIDENT_FORMAT: &str = "rss=";

/// The format asking for CPU time alone, as a clock, on the same terms.
const CPU_FORMAT: &str = "time=";

/// The bytes in the kibibyte `ps` reports resident memory in, on both
/// platforms this runs on.
const KIBIBYTE: u64 = 1024;

/// The seconds in a minute of a clock field.
const SECONDS_PER_MINUTE: u64 = 60;

/// The seconds in an hour of a clock field.
const SECONDS_PER_HOUR: u64 = 3600;

/// The digits a fraction of a second is padded to: nanoseconds.
const FRACTION_DIGITS: usize = 9;

/// Why a metric could not be read.
#[derive(Debug)]
pub enum MetricsError {
    /// `ps` could not be run, or said nothing about the process: an unknown
    /// process id lands here.
    Read {
        /// The process asked about.
        process_id: u32,
        /// What the operating system said, or what `ps` said instead of a
        /// number.
        source: io::Error,
    },
    /// `ps` answered with something that is not the number asked for.
    Parse {
        /// The process asked about.
        process_id: u32,
        /// What was wrong.
        detail: String,
    },
}

impl Display for MetricsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            MetricsError::Read { process_id, source } => write!(
                formatter,
                "process {process_id}: could not be read: {source}"
            ),
            MetricsError::Parse { process_id, detail } => write!(
                formatter,
                "process {process_id}: is not as expected: {detail}"
            ),
        }
    }
}

impl std::error::Error for MetricsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MetricsError::Read { source, .. } => Some(source),
            MetricsError::Parse { .. } => None,
        }
    }
}

/// One field of a process, as `ps` prints it for `format`.
///
/// # Errors
///
/// [`MetricsError::Read`] when `ps` cannot be run, says nothing for the
/// process, or exits non-zero — an unknown process id is all three.
fn ask(process_id: u32, format: &str) -> Result<String, MetricsError> {
    let output = Command::new(PS)
        .args([PROCESS_FLAG, &process_id.to_string(), FORMAT_FLAG, format])
        .output()
        .map_err(|source| MetricsError::Read { process_id, source })?;
    let answered = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && !answered.is_empty() {
        return Ok(answered);
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(MetricsError::Read {
        process_id,
        source: io::Error::other(if said.is_empty() {
            "no such process".to_owned()
        } else {
            said
        }),
    })
}

/// The resident memory of a process in bytes.
///
/// # Errors
///
/// [`MetricsError::Read`] for an unknown process, and
/// [`MetricsError::Parse`] for a reading that is not a number.
pub fn resident_memory(process_id: u32) -> Result<u64, MetricsError> {
    let answered = ask(process_id, RESIDENT_FORMAT)?;
    let kibibytes = answered
        .parse::<u64>()
        .map_err(|_unparsed| MetricsError::Parse {
            process_id,
            detail: format!("`{answered}` is not a number of kibibytes"),
        })?;
    Ok(kibibytes.saturating_mul(KIBIBYTE))
}

/// The CPU time a process has used in user and kernel mode, as `ps` reports
/// it.
///
/// # Errors
///
/// [`MetricsError::Read`] for an unknown process, and
/// [`MetricsError::Parse`] for a reading that is not a clock.
pub fn cpu_time(process_id: u32) -> Result<Duration, MetricsError> {
    let answered = ask(process_id, CPU_FORMAT)?;
    clock(process_id, &answered)
}

/// A clock as `ps` prints one — `HH:MM:SS.ss` on macOS, `HH:MM:SS` on Linux,
/// minutes and hours omitted while zero — turned into a duration.
///
/// # Errors
///
/// [`MetricsError::Parse`] when a field is not a number, or the fraction is
/// longer than a second can hold.
fn clock(process_id: u32, printed: &str) -> Result<Duration, MetricsError> {
    let unreadable = |what: &str| MetricsError::Parse {
        process_id,
        detail: format!("`{printed}` has no {what}"),
    };
    let (whole, fraction) = printed.split_once('.').unwrap_or((printed, ""));
    let mut fields = whole.rsplit(':');
    let seconds = fields
        .next()
        .and_then(|field| field.trim().parse::<u64>().ok())
        .ok_or_else(|| unreadable("seconds"))?;
    let minutes = fields
        .next()
        .map(|field| field.trim().parse::<u64>())
        .transpose()
        .map_err(|_unparsed| unreadable("minutes"))?
        .unwrap_or_default();
    let hours = fields
        .next()
        .map(|field| field.trim().parse::<u64>())
        .transpose()
        .map_err(|_unparsed| unreadable("hours"))?
        .unwrap_or_default();
    if fields.next().is_some() {
        return Err(unreadable("clock of hours, minutes and seconds"));
    }
    let total = seconds
        .saturating_add(minutes.saturating_mul(SECONDS_PER_MINUTE))
        .saturating_add(hours.saturating_mul(SECONDS_PER_HOUR));
    let padded = format!("{fraction:0<FRACTION_DIGITS$}");
    let nanoseconds = padded
        .parse::<u32>()
        .map_err(|_unparsed| unreadable("fraction of a second"))?;
    Duration::from_secs(total)
        .checked_add(Duration::from_nanos(u64::from(nanoseconds)))
        .ok_or_else(|| unreadable("duration a clock can hold"))
}
