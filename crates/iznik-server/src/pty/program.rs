//! The foreground program and directory of a pane's process, read from the
//! operating system so a tab can name itself without shell integration.
//!
//! Linux reads `/proc`. Other Unix hosts ask `ps` for the program and `lsof`
//! for the directory. Windows has neither in a form this process can ask
//! without a new dependency, so a Windows pane keeps the title and directory
//! the program reports itself.

use std::collections::BTreeMap;
use std::time::Duration;

use iznik_protocol::program::program_file_name;

/// How long a `ps` or `lsof` listing may take before it is abandoned.
///
/// A second is enough for a filtered listing and short enough that a stuck
/// tool cannot hold the next sample. The following interval tries again.
/// Linux reads `/proc` and does not run these tools.
#[cfg(all(unix, not(target_os = "linux")))]
const PROGRAM_READ_DEADLINE: Duration = Duration::from_secs(1);

/// How often the daemon samples every pane.
///
/// One second is off the output path — a pane that is printing is not waiting
/// on this — and short enough that starting a program or changing directory
/// shows up on the tab before it feels late.
pub const PROGRAM_INTERVAL: Duration = Duration::from_secs(1);

/// The program `ps` is asked to run.
#[cfg(all(unix, not(target_os = "linux")))]
const PS_PROGRAM: &str = "ps";

/// `ps` selects processes by id.
#[cfg(all(unix, not(target_os = "linux")))]
const PS_PROCESS_FLAG: &str = "-p";

/// `ps` selects the columns it prints.
#[cfg(all(unix, not(target_os = "linux")))]
const PS_FORMAT_FLAG: &str = "-o";

/// Process id and command, unheaded, so the line is the two fields.
#[cfg(all(unix, not(target_os = "linux")))]
const PS_FORMAT: &str = "pid=,comm=";

/// The program `lsof` is asked to run.
#[cfg(all(unix, not(target_os = "linux")))]
const LIST_PROGRAM: &str = "lsof";

/// `lsof` combines the following selectors with and.
#[cfg(all(unix, not(target_os = "linux")))]
const LIST_AND_FLAG: &str = "-a";

/// `lsof` selects processes by id.
#[cfg(all(unix, not(target_os = "linux")))]
const LIST_PROCESS_FLAG: &str = "-p";

/// `lsof` selects one file by its descriptor name.
#[cfg(all(unix, not(target_os = "linux")))]
const LIST_FILE_FLAG: &str = "-d";

/// The current-directory descriptor.
#[cfg(all(unix, not(target_os = "linux")))]
const LIST_DIRECTORY: &str = "cwd";

/// `lsof` prints machine-readable fields, one per line.
#[cfg(all(unix, not(target_os = "linux")))]
const LIST_FIELDS_FLAG: &str = "-Fn";

/// What one process was found to be running, and where, when either is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramSample {
    /// The program's file name, without a directory and without a Windows
    /// `.exe` ending.
    pub program: String,
    /// The process's directory, when it could be read.
    pub directory: Option<String>,
}

/// The program and directory of each live process in `process_ids`.
///
/// A process that cannot be read is absent. An empty list asks nothing.
pub async fn read_programs(process_ids: &[u32]) -> BTreeMap<u32, ProgramSample> {
    if process_ids.is_empty() {
        return BTreeMap::new();
    }
    #[cfg(target_os = "linux")]
    {
        read_linux(process_ids)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        read_listed(process_ids).await
    }
    #[cfg(windows)]
    {
        let _process_ids = process_ids;
        BTreeMap::new()
    }
}

/// `/proc/<pid>/exe` and `/proc/<pid>/cwd` for each process.
#[cfg(target_os = "linux")]
fn read_linux(process_ids: &[u32]) -> BTreeMap<u32, ProgramSample> {
    let mut samples = BTreeMap::new();
    for process_id in process_ids {
        let Some(sample) = read_linux_process(*process_id) else {
            continue;
        };
        samples.insert(*process_id, sample);
    }
    samples
}

/// One process from `/proc`, or nothing when it has already ended.
#[cfg(target_os = "linux")]
fn read_linux_process(process_id: u32) -> Option<ProgramSample> {
    let binary = std::fs::read_link(format!("/proc/{process_id}/exe")).ok()?;
    let program = binary.file_name()?.to_str()?.to_owned();
    let directory = std::fs::read_link(format!("/proc/{process_id}/cwd"))
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned));
    Some(ProgramSample {
        program: program_file_name(&program).to_owned(),
        directory,
    })
}

/// `ps` for the names and `lsof` for the directories.
#[cfg(all(unix, not(target_os = "linux")))]
async fn read_listed(process_ids: &[u32]) -> BTreeMap<u32, ProgramSample> {
    let listed = process_list(process_ids);
    let names = command_text(
        PS_PROGRAM,
        &[PS_PROCESS_FLAG, &listed, PS_FORMAT_FLAG, PS_FORMAT],
    )
    .await
    .map(|text| parse_ps(&text))
    .unwrap_or_default();
    let paths = command_text(
        LIST_PROGRAM,
        &[
            LIST_AND_FLAG,
            LIST_PROCESS_FLAG,
            &listed,
            LIST_FILE_FLAG,
            LIST_DIRECTORY,
            LIST_FIELDS_FLAG,
        ],
    )
    .await
    .map(|text| parse_directories(&text))
    .unwrap_or_default();
    let mut samples = BTreeMap::new();
    for process_id in process_ids {
        let Some(program) = names.get(process_id) else {
            continue;
        };
        samples.insert(
            *process_id,
            ProgramSample {
                program: program_file_name(program).to_owned(),
                directory: paths.get(process_id).cloned(),
            },
        );
    }
    samples
}

/// The comma-separated process list `ps` and `lsof` both take.
#[cfg(all(unix, not(target_os = "linux")))]
fn process_list(process_ids: &[u32]) -> String {
    let mut listed = String::new();
    for process_id in process_ids {
        if !listed.is_empty() {
            listed.push(',');
        }
        listed.push_str(&process_id.to_string());
    }
    listed
}

/// The standard output of `program` with `arguments`, or nothing when it
/// cannot be run or does not finish before [`PROGRAM_READ_DEADLINE`].
#[cfg(all(unix, not(target_os = "linux")))]
async fn command_text(program: &str, arguments: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(program);
    command.args(arguments).kill_on_drop(true);
    let collected = tokio::time::timeout(PROGRAM_READ_DEADLINE, command.output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&collected.stdout).into_owned())
}

/// `pid` and command from `ps -o pid=,comm=`.
#[cfg(all(unix, not(target_os = "linux")))]
fn parse_ps(text: &str) -> BTreeMap<u32, String> {
    let mut names = BTreeMap::new();
    for line in text.lines() {
        let Some((process, command)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(process_id) = process.parse::<u32>() else {
            continue;
        };
        let command = command.trim();
        if !command.is_empty() {
            names.insert(process_id, command.to_owned());
        }
    }
    names
}

/// Directories from `lsof -Fn`: a `p` line names the process and a following
/// `n` line is its directory.
#[cfg(all(unix, not(target_os = "linux")))]
fn parse_directories(text: &str) -> BTreeMap<u32, String> {
    let mut paths = BTreeMap::new();
    let mut process_id = None;
    for line in text.lines() {
        if let Some(process) = line.strip_prefix('p') {
            process_id = process.parse().ok();
        } else if let Some(path) = line.strip_prefix('n')
            && let Some(process_id) = process_id
            && !path.is_empty()
        {
            paths.insert(process_id, path.to_owned());
        }
    }
    paths
}
