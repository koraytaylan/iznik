//! Resident memory and CPU time of a process: the one implementation behind
//! every memory ceiling, every "memory does not grow" assertion and every
//! "costs no CPU" assertion in the workspace.
//!
//! Resident memory is asked of `ps` on every platform, which prints kibibytes
//! the same way. CPU time on Linux is the sum of user and system ticks in
//! `/proc/<pid>/stat`. `ps` there prints that clock as whole seconds, so a
//! spin shorter than a second was reported as no time at all. On other
//! platforms CPU time is the `ps` clock, which carries a fraction of a second.

use core::fmt::{self, Display, Formatter};
use std::io;
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
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
#[cfg(not(target_os = "linux"))]
const CPU_FORMAT: &str = "time=";

/// The bytes in the kibibyte `ps` reports resident memory in, on both
/// platforms this runs on.
const KIBIBYTE: u64 = 1024;

/// The seconds in a minute of a clock field.
#[cfg(not(target_os = "linux"))]
const SECONDS_PER_MINUTE: u64 = 60;

/// The seconds in an hour of a clock field.
#[cfg(not(target_os = "linux"))]
const SECONDS_PER_HOUR: u64 = 3600;

/// The digits a fraction of a second is padded to: nanoseconds.
#[cfg(not(target_os = "linux"))]
const FRACTION_DIGITS: usize = 9;

/// Nanoseconds in one second, so a tick count becomes a [`Duration`].
#[cfg(target_os = "linux")]
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

/// `utime` in `/proc/<pid>/stat`, counting fields after the command name.
///
/// The command is wrapped in parentheses and may itself contain spaces and
/// parentheses, so the fields that follow are counted from the last `)`.
#[cfg(target_os = "linux")]
const USER_TICKS_FIELD: usize = 11;

/// `stime`, the field immediately after [`USER_TICKS_FIELD`].
#[cfg(target_os = "linux")]
const SYSTEM_TICKS_FIELD: usize = 12;

/// The program that reports how many clock ticks a second is.
#[cfg(target_os = "linux")]
const TICKS_PROGRAM: &str = "getconf";

/// The argument that asks [`TICKS_PROGRAM`] for `CLK_TCK`.
#[cfg(target_os = "linux")]
const TICKS_ARGUMENT: &str = "CLK_TCK";

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

/// The CPU time a process has used in user and kernel mode.
///
/// On Linux this is user plus system ticks from `/proc/<pid>/stat`. Everywhere
/// else it is the `ps` clock.
///
/// # Errors
///
/// [`MetricsError::Read`] for an unknown process, and
/// [`MetricsError::Parse`] for a reading that is not ticks or a clock.
pub fn cpu_time(process_id: u32) -> Result<Duration, MetricsError> {
    #[cfg(target_os = "linux")]
    {
        linux_cpu_time(process_id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let answered = ask(process_id, CPU_FORMAT)?;
        clock(process_id, &answered)
    }
}

/// User and system time from `/proc/<pid>/stat`.
///
/// # Errors
///
/// [`MetricsError::Read`] when the stat file or the tick rate cannot be read,
/// and [`MetricsError::Parse`] when the file is not a stat line or the rate
/// is not a positive number of ticks.
#[cfg(target_os = "linux")]
fn linux_cpu_time(process_id: u32) -> Result<Duration, MetricsError> {
    let text = read_process_stat(process_id)?;
    let ticks = process_ticks(process_id, &text)?;
    let per_second = ticks_per_second(process_id)?;
    ticks_as_duration(process_id, ticks, per_second)
}

/// The text of `/proc/<pid>/stat`.
///
/// # Errors
///
/// [`MetricsError::Read`] when the process does not exist or the file cannot
/// be read.
#[cfg(target_os = "linux")]
fn read_process_stat(process_id: u32) -> Result<String, MetricsError> {
    std::fs::read_to_string(format!("/proc/{process_id}/stat"))
        .map_err(|source| MetricsError::Read { process_id, source })
}

/// User ticks plus system ticks from a `/proc/<pid>/stat` line.
///
/// # Errors
///
/// [`MetricsError::Parse`] when either field is absent or their sum overflows.
#[cfg(target_os = "linux")]
fn process_ticks(process_id: u32, text: &str) -> Result<u64, MetricsError> {
    let user = stat_field(text, USER_TICKS_FIELD);
    let system = stat_field(text, SYSTEM_TICKS_FIELD);
    match (user, system) {
        (Some(user_ticks), Some(system_ticks)) => user_ticks
            .checked_add(system_ticks)
            .ok_or_else(|| ticks_missing(process_id, text)),
        _missing => Err(ticks_missing(process_id, text)),
    }
}

/// One whitespace field after the command name in a stat line.
#[cfg(target_os = "linux")]
fn stat_field(text: &str, index: usize) -> Option<u64> {
    let command_end = text.rfind(')')?;
    let after = command_end.checked_add(1)?;
    let fields = text.get(after..)?;
    fields.split_whitespace().nth(index)?.parse().ok()
}

/// Why a stat line could not be turned into ticks.
#[cfg(target_os = "linux")]
fn ticks_missing(process_id: u32, text: &str) -> MetricsError {
    MetricsError::Parse {
        process_id,
        detail: format!("`{text}` has no user and system ticks"),
    }
}

/// Clock ticks in one second, asked of `getconf` once per process.
///
/// # Errors
///
/// [`MetricsError::Read`] when `getconf` cannot be run, and
/// [`MetricsError::Parse`] when it does not print a positive number.
#[cfg(target_os = "linux")]
fn ticks_per_second(process_id: u32) -> Result<u64, MetricsError> {
    static RATE: OnceLock<u64> = OnceLock::new();
    if let Some(rate) = RATE.get() {
        return Ok(*rate);
    }
    let rate = read_clock_ticks(process_id)?;
    Ok(*RATE.get_or_init(|| rate))
}

/// The number `getconf CLK_TCK` prints.
///
/// # Errors
///
/// [`MetricsError::Read`] when the program cannot be run or exits non-zero,
/// and [`MetricsError::Parse`] when the output is not a positive number.
#[cfg(target_os = "linux")]
fn read_clock_ticks(process_id: u32) -> Result<u64, MetricsError> {
    let output = Command::new(TICKS_PROGRAM)
        .arg(TICKS_ARGUMENT)
        .output()
        .map_err(|source| MetricsError::Read { process_id, source })?;
    if !output.status.success() {
        return Err(MetricsError::Read {
            process_id,
            source: io::Error::other("the clock tick rate was not reported"),
        });
    }
    let printed = String::from_utf8_lossy(&output.stdout);
    let rate = printed
        .trim()
        .parse::<u64>()
        .map_err(|_unparsed| MetricsError::Parse {
            process_id,
            detail: format!("`{}` is not a clock tick rate", printed.trim()),
        })?;
    if rate == 0 {
        return Err(MetricsError::Parse {
            process_id,
            detail: "a clock tick rate of zero cannot time a process".to_owned(),
        });
    }
    Ok(rate)
}

/// `ticks` at `per_second` as a duration, truncating to a whole nanosecond.
///
/// # Errors
///
/// [`MetricsError::Parse`] when the conversion overflows, which includes a
/// tick rate of zero.
#[cfg(target_os = "linux")]
fn ticks_as_duration(
    process_id: u32,
    ticks: u64,
    per_second: u64,
) -> Result<Duration, MetricsError> {
    let nanoseconds = ticks
        .checked_mul(NANOSECONDS_PER_SECOND)
        .and_then(|whole| whole.checked_div(per_second))
        .ok_or_else(|| MetricsError::Parse {
            process_id,
            detail: format!("{ticks} ticks at {per_second} per second is not a duration"),
        })?;
    Ok(Duration::from_nanos(nanoseconds))
}

/// A clock as `ps` prints one — `HH:MM:SS.ss`, minutes and hours omitted while
/// zero — turned into a duration. Linux does not use this: its `ps` clock has
/// no fraction, so CPU time is read from `/proc` instead.
///
/// # Errors
///
/// [`MetricsError::Parse`] when a field is not a number, or the fraction is
/// longer than a second can hold.
#[cfg(not(target_os = "linux"))]
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
