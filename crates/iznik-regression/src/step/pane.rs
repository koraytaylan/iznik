//! The `pane` step: the fidelity argument driven on a real `Pane` and checked
//! through the VT oracle, on the static musl binary inside the fixture.
//!
//! `corpus = "constructs"` drives every fidelity construct — a raw writer of the
//! construct's bytes on a pane — and checks either that the bytes reach history
//! byte-identical (`check = "bytes"`) or that the serialized screen reproduces
//! them on a fresh emulator (`check = "screen"`). `corpus = "flood"` drives a
//! sixty-four mebibyte flood and checks the ring holds its tail faithfully within
//! its capacity and its memory ceiling. A construct or flood that fails is a
//! non-zero exit with the reason, not a harness error.

use std::future::Future;
use std::time::{Duration, Instant};

use iznik_protocol::identity::Sequence;
use iznik_server::pane::Pane;
use iznik_server::pty::spawn::{Program, SpawnOptions};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::corpus::{self, constructs};
use iznik_testkit::vt::Vt;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::step::{Context, Outcome, StepError};

/// The default pane width.
const DEFAULT_COLUMNS: u16 = 80;
/// The default pane height.
const DEFAULT_ROWS: u16 = 24;
/// A history ring larger than any construct, so a construct never ages out.
const CONSTRUCT_HISTORY_BYTES: usize = 128 * 1024 * 1024;
/// The flood's history ring, smaller than the flood so the drop path is taken.
const FLOOD_HISTORY_BYTES: usize = 8 * 1024 * 1024;
/// The flood's size.
const FLOOD_BYTES: usize = 64 * 1024 * 1024;
/// The generator seed the flood is written and verified from.
const FLOOD_SEED: u64 = 0xf100d;
/// The resident-memory ceiling the flood's steady state must stay under, proving
/// the ring bounds memory rather than holding the whole flood.
const MEMORY_CEILING_KIB: u64 = 131_072;
/// How many times a settle poll reads the newest sequence before giving up.
const SETTLE_ATTEMPTS: usize = 800;
/// How long a settle poll waits between reads.
const SETTLE_INTERVAL: Duration = Duration::from_millis(25);
/// Where a construct's bytes are written for the pane to read.
const CONSTRUCT_FILE: &str = "/tmp/iznik-pane-construct.bin";
/// Where the flood's bytes are written for the pane to read.
const FLOOD_FILE: &str = "/tmp/iznik-pane-flood.bin";

/// The `pane` step. Its body is the `[steps.pane]` table: `corpus`
/// (`"constructs"` or `"flood"`), `check` (`"bytes"` or `"screen"`, for
/// constructs), and optional `columns`/`rows`.
///
/// # Errors
///
/// [`StepError::Malformed`] when the body is not the table this expects.
pub fn execute(
    _context: &Context,
    body: &toml::Value,
    timeout: Duration,
) -> Result<Outcome, StepError> {
    let table = body
        .as_table()
        .ok_or_else(|| malformed("a `pane` step's value is a table"))?;
    let corpus = required_str(table, "corpus")?;
    let columns = dimension(table, "columns", DEFAULT_COLUMNS)?;
    let rows = dimension(table, "rows", DEFAULT_ROWS)?;

    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| StepError::Input { source })?;

    let started = Instant::now();
    let checked = match corpus.as_str() {
        "constructs" => {
            let check = required_str(table, "check")?;
            runtime.block_on(with_deadline(
                timeout,
                drive_constructs(check, columns, rows),
            ))
        }
        "flood" => runtime.block_on(with_deadline(timeout, drive_flood(columns, rows))),
        other => return Err(malformed(format!("unknown `corpus` {other:?}"))),
    };
    let duration = started.elapsed();

    Ok(match checked {
        Ok(summary) => Outcome {
            exit: Some(0),
            timed_out: false,
            duration,
            stdout: summary,
            stderr: String::new(),
        },
        Err(reason) => Outcome {
            exit: Some(1),
            timed_out: false,
            duration,
            stdout: String::new(),
            stderr: reason,
        },
    })
}

/// A malformed-step error with a message.
fn malformed(detail: impl Into<String>) -> StepError {
    StepError::Malformed {
        detail: detail.into(),
    }
}

/// A required string field of the table.
///
/// # Errors
///
/// [`StepError::Malformed`] when it is absent or not a string.
fn required_str(table: &toml::Table, key: &str) -> Result<String, StepError> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("no string `{key}`")))
}

/// A `u16` dimension field, or `fallback` when it is absent.
///
/// # Errors
///
/// [`StepError::Malformed`] when it is present but not a `u16`.
fn dimension(table: &toml::Table, key: &str, fallback: u16) -> Result<u16, StepError> {
    match table.get(key) {
        None => Ok(fallback),
        Some(value) => value
            .as_integer()
            .and_then(|integer| u16::try_from(integer).ok())
            .ok_or_else(|| malformed(format!("`{key}` is not a u16"))),
    }
}

/// Runs `work` under `timeout`, mapping a timeout to a failure reason.
///
/// # Errors
///
/// The `work`'s own failure reason, or a deadline message when it runs too long.
async fn with_deadline(
    timeout: Duration,
    work: impl Future<Output = Result<String, String>>,
) -> Result<String, String> {
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_elapsed) => Err("the pane step ran past its deadline".to_owned()),
    }
}

/// Spawn options for a pane whose `sh` turns off output post-processing and echo,
/// writes `path`, then idles alive so its screen stays queryable.
fn raw_writer(path: &str, columns: u16, rows: u16) -> SpawnOptions {
    let script = format!("stty -opost -echo 2>/dev/null; cat {path}; exec sleep 3600");
    SpawnOptions {
        program: Program::Command {
            path: "sh".into(),
            arguments: vec!["-c".to_owned(), script],
        },
        columns,
        rows,
        working_directory: None,
        terminfo_directory: None,
    }
}

/// Waits until the pane's newest sequence stops advancing, and returns it.
async fn settle(pane: &Pane) -> Sequence {
    let mut last = pane.state().newest;
    for _ in 0..SETTLE_ATTEMPTS {
        tokio::time::sleep(SETTLE_INTERVAL).await;
        let now = pane.state().newest;
        if now == last {
            return now;
        }
        last = now;
    }
    last
}

/// The offset of the first differing byte, or `None` when the two are identical.
fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    let common = left.len().min(right.len());
    for index in 0..common {
        if left.get(index) != right.get(index) {
            return Some(index);
        }
    }
    if left.len() == right.len() {
        None
    } else {
        Some(common)
    }
}

/// A snapshot's screen layout — everything but the per-cell style attributes the
/// formatter reconstructs imperfectly and the title the serializer omits.
fn layout(snapshot: &str) -> String {
    snapshot
        .lines()
        .take_while(|line| *line != "attributes")
        .filter(|line| !line.starts_with("title "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drives every construct on a raw-writer pane and checks `check`.
///
/// # Errors
///
/// A failure reason for the first construct that is not byte-identical or does
/// not reproduce.
async fn drive_constructs(check: String, columns: u16, rows: u16) -> Result<String, String> {
    let thread = MirrorThread::start().map_err(|error| format!("the mirror thread: {error}"))?;
    let mut proven = 0_u32;
    for construct in constructs() {
        // Graphics are images a client re-requests, not the text screen this
        // proves; the serializer skips them and so does this.
        if construct.name.contains("graphics") {
            continue;
        }
        let content: Vec<u8> = construct.chunks.concat();
        std::fs::write(CONSTRUCT_FILE, &content)
            .map_err(|error| format!("writing {}: {error}", construct.name))?;
        let pane = Pane::spawn(
            &raw_writer(CONSTRUCT_FILE, columns, rows),
            CONSTRUCT_HISTORY_BYTES,
            &thread,
        )
        .await
        .map_err(|error| format!("spawning for {}: {error}", construct.name))?;
        settle(&pane).await;

        match check.as_str() {
            "bytes" => {
                let history = pane
                    .read_history(Sequence(0))
                    .map_err(|error| format!("history for {}: {error}", construct.name))?;
                if let Some(offset) = first_difference(&history, &content) {
                    return Err(format!(
                        "construct {} differs at offset {offset}: history {} bytes, wrote {} bytes",
                        construct.name,
                        history.len(),
                        content.len()
                    ));
                }
            }
            "screen" => {
                if let Err(reason) = reproduces(&pane, &content, columns, rows).await {
                    return Err(format!("construct {}: {reason}", construct.name));
                }
            }
            other => return Err(format!("unknown `check` {other:?}")),
        }
        proven = proven.saturating_add(1);
    }
    let _removed = std::fs::remove_file(CONSTRUCT_FILE);
    Ok(format!("{proven} constructs checked for {check}"))
}

/// Whether the pane's screen reproduces `content` on a fresh emulator.
///
/// # Errors
///
/// A reason when the reproduction's layout differs from the oracle's.
async fn reproduces(pane: &Pane, content: &[u8], columns: u16, rows: u16) -> Result<(), String> {
    let screen = pane
        .screen()
        .await
        .map_err(|error| format!("screen: {error}"))?;
    let mut reproduced =
        Vt::new(screen.columns, screen.rows).map_err(|error| format!("reproduction: {error}"))?;
    reproduced.feed(&screen.bytes);
    let mut direct = Vt::new(columns, rows).map_err(|error| format!("direct oracle: {error}"))?;
    direct.feed(content);
    let left = layout(
        &reproduced
            .snapshot()
            .map_err(|error| format!("reproduced snapshot: {error}"))?,
    );
    let right = layout(
        &direct
            .snapshot()
            .map_err(|error| format!("direct snapshot: {error}"))?,
    );
    if left == right {
        Ok(())
    } else {
        Err("the screen does not reproduce".to_owned())
    }
}

/// Drives a flood and checks the ring holds its tail faithfully, bounds it to
/// its capacity, and keeps resident memory under the ceiling.
///
/// # Errors
///
/// A reason when the flood is not held faithfully or memory is over the ceiling.
async fn drive_flood(columns: u16, rows: u16) -> Result<String, String> {
    let thread = MirrorThread::start().map_err(|error| format!("the mirror thread: {error}"))?;
    let content = corpus::generated(FLOOD_SEED, FLOOD_BYTES);
    std::fs::write(FLOOD_FILE, &content).map_err(|error| format!("writing the flood: {error}"))?;

    let pane = Pane::spawn(
        &raw_writer(FLOOD_FILE, columns, rows),
        FLOOD_HISTORY_BYTES,
        &thread,
    )
    .await
    .map_err(|error| format!("spawning the flood: {error}"))?;
    let newest = settle(&pane).await;
    let _removed = std::fs::remove_file(FLOOD_FILE);

    if newest.0 != u64::try_from(FLOOD_BYTES).unwrap_or(u64::MAX) {
        return Err(format!(
            "newest {} is not the flood length {FLOOD_BYTES}",
            newest.0
        ));
    }
    let state = pane.state();
    let held = newest.0.saturating_sub(state.oldest.0);
    if held > u64::try_from(FLOOD_HISTORY_BYTES).unwrap_or(u64::MAX) {
        return Err(format!(
            "the ring holds {held}, past its capacity {FLOOD_HISTORY_BYTES}"
        ));
    }
    // Keep only the tail the ring should hold, then drop the whole flood before
    // measuring memory, so the ceiling reflects the ring's steady state rather
    // than this step's own generation of the flood.
    let start = usize::try_from(state.oldest.0).unwrap_or(content.len());
    let expected_tail = content.get(start..).unwrap_or(&[]).to_vec();
    drop(content);
    let tail = pane
        .read_history(state.oldest)
        .map_err(|error| format!("history from the oldest held: {error}"))?;
    if let Some(offset) = first_difference(&tail, &expected_tail) {
        return Err(format!("the held tail differs at offset {offset}"));
    }
    let resident = resident_kib().map_err(|error| format!("reading memory: {error}"))?;
    if resident > MEMORY_CEILING_KIB {
        return Err(format!(
            "resident memory {resident} KiB is over the ceiling {MEMORY_CEILING_KIB} KiB"
        ));
    }
    Ok(format!("flood held {held} bytes, resident {resident} KiB"))
}

/// This process's current resident set size in kibibytes, from `/proc/self`.
///
/// # Errors
///
/// A reason when the status cannot be read or parsed.
fn resident_kib() -> Result<u64, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|error| format!("/proc/self/status: {error}"))?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let number = rest.split_whitespace().next().unwrap_or("");
            return number
                .parse::<u64>()
                .map_err(|error| format!("VmRSS {number:?}: {error}"));
        }
    }
    Err("no VmRSS line".to_owned())
}
