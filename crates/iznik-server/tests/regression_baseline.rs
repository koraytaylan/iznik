//! The baseline's ceilings, asserted rather than measured: generous enough
//! that a noisy machine does not fail them, strict enough that a real
//! regression does.
//!
//! Every case is ignored by default and measures over a window, so it is run
//! deliberately — `--run-ignored all` under the `regression` profile, which is
//! the profile the containers run. Every name carries `baseline`, so nextest's
//! own grouping gives them the machine.
//!
//! The measuring is the benchmark's, included here rather than written twice:
//! one definition of how a figure is taken, printed there for a person and
//! held to a ceiling here.

#[path = "../benches/baseline.rs"]
pub mod baseline;

use std::path::PathBuf;
use std::time::Duration;

use baseline::{
    FIFTY_PANE_MEMORY_CEILING, FLOOD_LATENCY_CEILING, Failed, Figures, IDLE_LATENCY_CEILING,
    RESTING_MEMORY_CEILING, SINGLE_PANE_THROUGHPUT_FLOOR, latency, memory, panes_throughput,
    startup, table,
};
use iznik_testkit::stack::STARTUP_CEILING;

/// The percentile every latency ceiling is stated at, over [`OF`].
const AT: usize = 99;

/// What it is a percentile of.
const OF: usize = 100;

/// How many panes the aggregate throughput is taken across.
const AGGREGATE_PANES: usize = 8;

/// How many panes the second memory figure is taken with.
const MANY_PANES: usize = 50;

/// The committed table, relative to this crate.
const NOTES: &str = "../../docs/notes/baseline.md";

/// The value `upper` parts in `lower` of the way through a sorted set.
fn percentile(sorted: &[Duration], upper: usize, lower: usize) -> Duration {
    let last = sorted.len().saturating_sub(1);
    sorted
        .len()
        .saturating_mul(upper)
        .checked_div(lower)
        .and_then(|at| sorted.get(at.min(last)))
        .copied()
        .unwrap_or_default()
}

/// # Panics
///
/// When a keystroke's round trip with nothing else happening is slower than
/// [`IDLE_LATENCY_CEILING`] at the ninety-ninth percentile.
#[ignore = "measures over a window; run deliberately under the regression profile"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baseline_idle_latency_is_under_its_ceiling() {
    let case = async {
        let trips = latency(0).await?;
        let tail = percentile(&trips, AT, OF);
        assert!(
            tail < IDLE_LATENCY_CEILING,
            "p{AT} {tail:?} against {IDLE_LATENCY_CEILING:?} (median {:?}, worst {:?})",
            percentile(&trips, 50, OF),
            trips.last()
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a keystroke's round trip beside a flooding pane is slower than
/// [`FLOOD_LATENCY_CEILING`] at the ninety-ninth percentile.
#[ignore = "measures over a window; run deliberately under the regression profile"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baseline_flood_latency_is_under_its_ceiling() {
    let case = async {
        let trips = latency(1).await?;
        let tail = percentile(&trips, AT, OF);
        assert!(
            tail < FLOOD_LATENCY_CEILING,
            "p{AT} {tail:?} against {FLOOD_LATENCY_CEILING:?} (median {:?}, worst {:?})",
            percentile(&trips, 50, OF),
            trips.last()
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When one pane does not sustain [`SINGLE_PANE_THROUGHPUT_FLOOR`], or eight
/// together do not sustain more than one.
#[ignore = "measures over a window; run deliberately under the regression profile"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baseline_throughput_is_over_its_floor() {
    let case = async {
        let single = panes_throughput(1).await?;
        assert!(
            single >= SINGLE_PANE_THROUGHPUT_FLOOR,
            "one pane sustained {single} bytes a second, under {SINGLE_PANE_THROUGHPUT_FLOOR}"
        );
        let together = panes_throughput(AGGREGATE_PANES).await?;
        assert!(
            together > single,
            "{AGGREGATE_PANES} panes sustained {together}, no more than one pane's {single}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the daemon's resident memory at rest is over
/// [`RESTING_MEMORY_CEILING`], or with fifty idle panes over
/// [`FIFTY_PANE_MEMORY_CEILING`].
#[ignore = "measures over a window; run deliberately under the regression profile"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baseline_memory_is_under_its_ceilings() {
    let case = async {
        let (resting, holding) = memory(MANY_PANES).await?;
        assert!(
            resting < RESTING_MEMORY_CEILING,
            "at rest {resting} bytes, over {RESTING_MEMORY_CEILING}"
        );
        assert!(
            holding < FIFTY_PANE_MEMORY_CEILING,
            "with {MANY_PANES} panes {holding} bytes, over {FIFTY_PANE_MEMORY_CEILING}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a daemon takes longer than [`STARTUP_CEILING`] to answer.
#[ignore = "measures over a window; run deliberately under the regression profile"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baseline_startup_is_under_its_ceiling() {
    let case = async {
        let taken = startup().await?;
        assert!(
            taken < STARTUP_CEILING,
            "it answered in {taken:?}, over {STARTUP_CEILING:?}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the committed table does not carry every figure the benchmark
/// reports, which is how a number in the notes goes stale unnoticed.
#[ignore = "reads the committed table; run with the rest of the baseline"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_notes_carry_every_figure() {
    let case = async {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(NOTES);
        let committed = std::fs::read_to_string(&path)?;
        let shape = table(&Figures {
            idle_middle: Duration::ZERO,
            idle_tail: Duration::ZERO,
            flood_middle: Duration::ZERO,
            flood_tail: Duration::ZERO,
            single_throughput: 0,
            aggregate_throughput: 0,
            resting_memory: 0,
            many_pane_memory: 0,
            startup: Duration::ZERO,
        });
        let missing: Vec<&str> = shape
            .lines()
            .filter_map(|row| row.split('|').nth(1))
            .map(str::trim)
            .filter(|named| !named.is_empty() && *named != "Figure")
            .filter(|named| !committed.contains(*named))
            .collect();
        assert!(
            missing.is_empty(),
            "{} does not carry {missing:?}",
            path.display()
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
