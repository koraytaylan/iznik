//! The performance baseline: input-to-echo latency, throughput, resident
//! memory and startup of the daemon, measured with `std::time::Instant` and
//! reported as a Markdown table.
//!
//! Every performance claim in this repository is a measured, committed number
//! and never an adjective. These are measured through the real binary over a
//! real unix socket, under the profile the containers run, so the table in
//! `docs/notes/baseline.md` describes what a person would get.
//!
//! The measuring lives here and the ceilings live in
//! `tests/regression_baseline.rs`, which includes this file: one definition of
//! how a figure is taken, two things done with it — printed for a person, and
//! held to a ceiling generous enough to survive a noisy machine.

use core::fmt::Write as _;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::identity::PaneId;
use iznik_protocol::model::HostModel;
use iznik_testkit::client::TestClient;
use iznik_testkit::metrics;
use iznik_testkit::stack::{DaemonMode, STARTUP_CEILING, Stack, StackOptions};
use tokio::net::UnixStream;

/// The most a keystroke may take to come back with nothing else happening.
pub const IDLE_LATENCY_CEILING: Duration = Duration::from_millis(5);

/// The most it may take at the ninety-ninth percentile with another pane
/// flooding at line rate.
pub const FLOOD_LATENCY_CEILING: Duration = Duration::from_millis(30);

/// The least one pane must sustain through the socket, in bytes a second.
pub const SINGLE_PANE_THROUGHPUT_FLOOR: u64 = 50 * 1024 * 1024;

/// The most resident memory a daemon holding nothing may occupy.
pub const RESTING_MEMORY_CEILING: u64 = 32 * 1024 * 1024;

/// The most it may occupy holding fifty idle panes.
pub const FIFTY_PANE_MEMORY_CEILING: u64 = 256 * 1024 * 1024;

/// How many round trips each latency figure is taken over.
const SAMPLES: usize = 1000;

/// The percentile the tail is reported at, over [`OF`].
pub const AT: usize = 99;

/// The middle, reported beside it.
pub const MIDDLE: usize = 50;

/// What both are percentiles of.
pub const OF: usize = 100;

/// How much one pane is flooded with when throughput is measured.
const THROUGHPUT_MEBIBYTES: usize = 16;

/// How many panes the aggregate figure is taken across.
pub const AGGREGATE_PANES: usize = 8;

/// How many panes the second memory figure is taken with.
pub const MANY_PANES: usize = 50;

/// The size every pane is made at.
const COLUMNS: u16 = 80;

/// Their height.
const ROWS: u16 = 24;

/// A mebibyte.
const MEBIBYTE: usize = 1024 * 1024;

/// How many milliseconds a second is, for the rates.
const MILLISECONDS_A_SECOND: u64 = 1000;

/// What the ceiling column says for a figure that is reported and not bounded.
const NO_CEILING: &str = "none";

/// How long a measurement waits for something that should already be there.
const PROMPT: Duration = Duration::from_secs(20);

/// How long it waits for something it does not expect.
const BRIEF: Duration = Duration::from_millis(200);

/// How many reads a measurement makes before it gives up.
const READ_ATTEMPTS: usize = 100_000;

/// How long one keystroke may take before the measurement says the pump
/// stopped rather than went slowly.
const SAMPLE_DEADLINE: Duration = Duration::from_secs(5);

/// Anything a measurement can fail on.
pub type Failed = Box<dyn std::error::Error>;

/// Every figure the baseline reports.
#[derive(Clone, Debug)]
pub struct Figures {
    /// Keystroke to echo at rest, in the middle.
    pub idle_middle: Duration,
    /// And at the tail.
    pub idle_tail: Duration,
    /// Keystroke to echo under a flood, in the middle.
    pub flood_middle: Duration,
    /// And at the tail.
    pub flood_tail: Duration,
    /// One pane's sustained bytes a second through the socket.
    pub single_throughput: u64,
    /// Eight panes' together.
    pub aggregate_throughput: u64,
    /// The daemon's resident bytes holding nothing.
    pub resting_memory: u64,
    /// And holding fifty idle panes.
    pub many_pane_memory: u64,
    /// How long from `--foreground` to a socket that answers.
    pub startup: Duration,
}

/// The value `upper` parts in `lower` of the way through a sorted set.
#[must_use]
pub fn percentile(sorted: &[Duration], upper: usize, lower: usize) -> Duration {
    let last = sorted.len().saturating_sub(1);
    sorted
        .len()
        .saturating_mul(upper)
        .checked_div(lower)
        .and_then(|at| sorted.get(at.min(last)))
        .copied()
        .unwrap_or_default()
}

/// A stack running the real binary, and a client that has shaken hands.
///
/// # Errors
///
/// When the daemon will not start or the handshake fails.
pub async fn attached() -> Result<(Stack, TestClient<UnixStream>), Failed> {
    let stack = Stack::start(StackOptions {
        daemon: DaemonMode::Binary(PathBuf::from(env!("CARGO_BIN_EXE_iznik-server"))),
        ..StackOptions::default()
    })
    .await?;
    let mut client = TestClient::connect(stack.socket()).await?;
    let _greeting = client.hello(Capabilities::from_bits(0)).await?;
    Ok((stack, client))
}

/// The daemon's process id, which it records in its lock file.
///
/// # Errors
///
/// When the lock names none.
pub fn daemon_of(stack: &Stack) -> Result<u32, Failed> {
    iznik_server::daemon::lock::holder(&stack.directory().join("server.lock"))
        .ok_or_else(|| "the daemon's lock names no process".into())
}

/// Makes a session and says which pane it made.
///
/// # Errors
///
/// When the command is refused, or the model holds no new pane.
pub async fn new_pane(client: &mut TestClient<UnixStream>) -> Result<PaneId, Failed> {
    let before = client.snapshot().await?;
    let outcome = client
        .command(SessionCommand::CreateSession {
            name: "bench".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        })
        .await?;
    if let CommandOutcome::Rejected { code, message } = outcome {
        return Err(format!("the session was refused ({code:?}): {message}").into());
    }
    let after = client.snapshot().await?;
    let held = panes_of(&before);
    panes_of(&after)
        .into_iter()
        .find(|pane| !held.contains(pane))
        .ok_or_else(|| "the model holds no new pane".into())
}

/// Every pane a model holds.
fn panes_of(model: &HostModel) -> Vec<PaneId> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .map(|pane| pane.id)
        .collect()
}

/// Asks a pane's shell for about `mebibytes` mebibytes of printable lines.
///
/// # Errors
///
/// When the pane will not take it.
pub async fn flood(
    client: &mut TestClient<UnixStream>,
    pane: PaneId,
    mebibytes: usize,
) -> Result<usize, Failed> {
    let line: String = core::iter::repeat_n('x', COLUMNS.into()).collect();
    let width = usize::from(COLUMNS).saturating_add(1);
    let wanted = MEBIBYTE.saturating_mul(mebibytes).saturating_add(width);
    let count = wanted.checked_div(width).unwrap_or_default();
    let asked = format!("yes {line} | head -n {count}\n");
    client.input(pane, asked.into_bytes()).await?;
    Ok(count.saturating_mul(width))
}

/// The keystroke-to-echo round trips of one pane, with `alongside` panes
/// flooding beside it.
///
/// # Errors
///
/// When a keystroke is never echoed.
pub async fn round_trips(
    client: &mut TestClient<UnixStream>,
    typed: PaneId,
    alongside: &[PaneId],
) -> Result<Vec<Duration>, Failed> {
    // `cat` on a pseudoterminal echoes a line when one arrives, which is the
    // smallest honest stand-in for a keystroke.
    client
        .input(typed, b"stty -opost -echo 2>/dev/null; cat\n".to_vec())
        .await?;
    client.auto_credit(true);
    client.subscribe(typed).await?;
    for pane in alongside {
        client.subscribe(*pane).await?;
        let line: String = core::iter::repeat_n('x', COLUMNS.into()).collect();
        let endless = format!("while :; do yes {line} | head -n 20000; done\n");
        client.input(*pane, endless.into_bytes()).await?;
    }
    // Everything the subscriptions and the setup produced, out of the way.
    for _attempt in 0..READ_ATTEMPTS {
        if client.next(BRIEF).await.is_err() {
            break;
        }
    }

    let mut trips = Vec::with_capacity(SAMPLES);
    for turn in 0..SAMPLES {
        let before = client.bytes_of(typed).len();
        let started = Instant::now();
        client
            .input(typed, format!("{turn}\n").into_bytes())
            .await?;
        loop {
            if client.next(SAMPLE_DEADLINE).await.is_err() {
                return Err(format!("keystroke {turn} was never echoed").into());
            }
            if client.bytes_of(typed).len() > before {
                break;
            }
        }
        trips.push(started.elapsed());
    }
    trips.sort_unstable();
    Ok(trips)
}

/// The bytes a second `panes` sustain together through the socket.
///
/// # Errors
///
/// When a pane never produces what it was asked for.
pub async fn throughput(
    client: &mut TestClient<UnixStream>,
    panes: &[PaneId],
) -> Result<u64, Failed> {
    client.auto_credit(true);
    for pane in panes {
        client.subscribe(*pane).await?;
    }
    // The clock starts before the first request, not after the last: bytes a
    // pane produced while the others were still being asked are bytes that
    // arrived during the measurement, and counting them against a shorter
    // window would make eight panes look faster than they are.
    let started = Instant::now();
    let mut wanted = 0_usize;
    for pane in panes {
        wanted = wanted.saturating_add(flood(client, *pane, THROUGHPUT_MEBIBYTES).await?);
    }
    let mut carried = 0_usize;
    for _attempt in 0..READ_ATTEMPTS {
        if carried >= wanted {
            break;
        }
        if client.next(PROMPT).await.is_err() {
            break;
        }
        carried = panes.iter().fold(0, |held, pane| {
            held.saturating_add(client.bytes_of(*pane).len())
        });
    }
    if carried < wanted {
        return Err(format!(
            "{} panes produced {carried} of the {wanted} bytes they were asked for",
            panes.len()
        )
        .into());
    }
    // In whole milliseconds and whole bytes: a rate is a ratio of two counts,
    // and a float would only add a conversion nothing here needs.
    let taken = u64::try_from(started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    let bytes = u64::try_from(carried).unwrap_or(u64::MAX);
    Ok(bytes
        .saturating_mul(MILLISECONDS_A_SECOND)
        .checked_div(taken)
        .unwrap_or_default())
}

/// The daemon's resident bytes holding nothing, and holding `panes` idle
/// panes.
///
/// # Errors
///
/// When the daemon cannot be started, found or asked for panes.
pub async fn memory(panes: usize) -> Result<(u64, u64), Failed> {
    let (stack, mut client) = attached().await?;
    let daemon = daemon_of(&stack)?;
    let resting = metrics::resident_memory(daemon)?;
    for _pane in 0..panes {
        let _made = new_pane(&mut client).await?;
    }
    let holding = metrics::resident_memory(daemon)?;
    Ok((resting, holding))
}

/// How long from asking for a stack to a socket that answers: a temporary
/// runtime directory made, the binary run with `--foreground`, and the first
/// connection accepted.
///
/// The directory is part of it because it is part of what a host does on its
/// first use, and separating the two would report a number nobody waits for.
///
/// # Errors
///
/// When the daemon will not start, or does not answer inside
/// [`STARTUP_CEILING`] — which is the ceiling, so a daemon slower than it
/// fails here rather than being reported as slow.
pub async fn startup() -> Result<Duration, Failed> {
    let started = Instant::now();
    let stack = Stack::start(StackOptions {
        daemon: DaemonMode::Binary(PathBuf::from(env!("CARGO_BIN_EXE_iznik-server"))),
        ..StackOptions::default()
    })
    .await?;
    let taken = started.elapsed();
    drop(stack);
    Ok(taken)
}

/// The round trips of one pane with `alongside` others flooding beside it,
/// each on a stack of its own.
///
/// # Errors
///
/// Whatever the measurement reports.
pub async fn latency(alongside: usize) -> Result<Vec<Duration>, Failed> {
    let (_stack, mut client) = attached().await?;
    let typed = new_pane(&mut client).await?;
    let mut beside = Vec::with_capacity(alongside);
    for _pane in 0..alongside {
        beside.push(new_pane(&mut client).await?);
    }
    round_trips(&mut client, typed, &beside).await
}

/// The bytes a second `count` panes sustain together.
///
/// # Errors
///
/// Whatever the measurement reports.
pub async fn panes_throughput(count: usize) -> Result<u64, Failed> {
    let (_stack, mut client) = attached().await?;
    let mut panes = Vec::with_capacity(count);
    for _pane in 0..count {
        panes.push(new_pane(&mut client).await?);
    }
    throughput(&mut client, &panes).await
}

/// Takes every figure.
///
/// # Errors
///
/// Whatever a measurement reports.
pub async fn measure() -> Result<Figures, Failed> {
    let idle = latency(0).await?;
    let flooded = latency(1).await?;
    let single = panes_throughput(1).await?;
    let aggregate = panes_throughput(AGGREGATE_PANES).await?;
    let (resting, holding) = memory(MANY_PANES).await?;
    Ok(Figures {
        idle_middle: percentile(&idle, MIDDLE, OF),
        idle_tail: percentile(&idle, AT, OF),
        flood_middle: percentile(&flooded, MIDDLE, OF),
        flood_tail: percentile(&flooded, AT, OF),
        single_throughput: single,
        aggregate_throughput: aggregate,
        resting_memory: resting,
        many_pane_memory: holding,
        startup: startup().await?,
    })
}

/// A duration in milliseconds, to three places.
fn milliseconds(taken: Duration) -> String {
    format!("{:.3} ms", taken.as_secs_f64() * 1000.0)
}

/// A byte rate in mebibytes a second.
fn rate(bytes: u64) -> String {
    let mebibytes = u32::try_from(bytes.saturating_div(1024 * 1024)).unwrap_or(u32::MAX);
    format!("{mebibytes} MiB/s")
}

/// A size in mebibytes.
fn size(bytes: u64) -> String {
    let mebibytes = u32::try_from(bytes.saturating_div(1024 * 1024)).unwrap_or(u32::MAX);
    format!("{mebibytes} MiB")
}

/// The figures as the Markdown table `docs/notes/baseline.md` carries.
#[must_use]
pub fn table(figures: &Figures) -> String {
    let mut said = String::from("| Figure | Measured | Ceiling |\n|---|---|---|\n");
    let rows = [
        (
            "Keystroke to echo, at rest, median",
            milliseconds(figures.idle_middle),
            String::from(NO_CEILING),
        ),
        (
            "Keystroke to echo, at rest, 99th percentile",
            milliseconds(figures.idle_tail),
            milliseconds(IDLE_LATENCY_CEILING),
        ),
        (
            "Keystroke to echo, under a flood, median",
            milliseconds(figures.flood_middle),
            String::from(NO_CEILING),
        ),
        (
            "Keystroke to echo, under a flood, 99th percentile",
            milliseconds(figures.flood_tail),
            milliseconds(FLOOD_LATENCY_CEILING),
        ),
        (
            "One pane's throughput",
            rate(figures.single_throughput),
            format!("at least {}", rate(SINGLE_PANE_THROUGHPUT_FLOOR)),
        ),
        (
            "Eight panes' throughput together",
            rate(figures.aggregate_throughput),
            String::from("more than one pane's"),
        ),
        (
            "Resident memory at rest",
            size(figures.resting_memory),
            size(RESTING_MEMORY_CEILING),
        ),
        (
            "Resident memory with fifty idle panes",
            size(figures.many_pane_memory),
            size(FIFTY_PANE_MEMORY_CEILING),
        ),
        (
            "Startup to a socket that answers",
            milliseconds(figures.startup),
            milliseconds(STARTUP_CEILING),
        ),
    ];
    for (named, measured, ceiling) in rows {
        // A row that will not format is a row nobody sees; there is nothing
        // else to do about it here.
        let _written = writeln!(said, "| {named} | {measured} | {ceiling} |");
    }
    said
}

/// The benchmark: measure, and print the table for a person to commit.
///
/// Cargo builds a benchmark with `cfg(test)` set, so nothing here can tell a
/// benchmark from the test that includes it; the test names this instead.
pub fn main() {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let said = match runtime.block_on(measure()) {
        Ok(figures) => table(&figures),
        Err(error) => format!("the baseline could not be measured: {error}\n"),
    };
    // Through the runtime, like every other thing this crate says: nothing
    // here blocks on the standard library's streams.
    runtime.block_on(async {
        let mut stdout = tokio::io::stdout();
        let _written = tokio::io::AsyncWriteExt::write_all(&mut stdout, said.as_bytes()).await;
        let _flushed = tokio::io::AsyncWriteExt::flush(&mut stdout).await;
    });
}
