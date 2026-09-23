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
    AGGREGATE_PANES, AT, CONCURRENT, CONCURRENT_OF, FIFTY_PANE_MEMORY_CEILING,
    FLOOD_LATENCY_CEILING, Failed, Figures, IDLE_LATENCY_CEILING, MANY_PANES, MIDDLE, OF,
    RESTING_MEMORY_CEILING, SHARED, SHARED_OF, SINGLE_PANE_THROUGHPUT_FLOOR, latency, memory,
    panes_throughput, percentile, startup, table,
};
use iznik_testkit::stack::STARTUP_CEILING;

/// The committed table, relative to this crate.
const NOTES: &str = "../../docs/notes/baseline.md";

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
            percentile(&trips, MIDDLE, OF),
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
            percentile(&trips, MIDDLE, OF),
            trips.last()
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When one pane does not sustain [`SINGLE_PANE_THROUGHPUT_FLOOR`]; when eight
/// together sustain less than [`SHARED`] tenths of what one does; or when, as
/// the first of the eight finishes its flood, the pane furthest behind has
/// delivered less than [`CONCURRENT`] in [`CONCURRENT_OF`] of its own — which
/// is what a scheduler that serves panes one after another looks like, and
/// what the aggregate rate alone cannot tell apart from sharing.
#[ignore = "measures over a window; run deliberately under the regression profile"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baseline_throughput_is_over_its_floor() {
    let case = async {
        let single = panes_throughput(1).await?.rate;
        assert!(
            single >= SINGLE_PANE_THROUGHPUT_FLOOR,
            "one pane sustained {single} bytes a second, under {SINGLE_PANE_THROUGHPUT_FLOOR}"
        );
        let together = panes_throughput(AGGREGATE_PANES).await?;
        // Not more than one pane's, but not less either: both saturate the
        // same socket, so which of the two comes out ahead is the machine
        // talking.
        assert!(
            together.rate.saturating_mul(SHARED_OF) >= single.saturating_mul(SHARED),
            "{AGGREGATE_PANES} panes sustained {}, under {SHARED}/{SHARED_OF} of one \
             pane's {single}",
            together.rate
        );
        // Serving the panes one after another would hold that rate too. What
        // it cannot do is move them forward together: when the first is done,
        // the others have been sent next to nothing.
        assert!(
            together.least_at_first_finish.saturating_mul(CONCURRENT_OF)
                >= together.each_wanted.saturating_mul(CONCURRENT),
            "when the first of {AGGREGATE_PANES} panes had delivered its {} bytes, the one \
             furthest behind had delivered {}, under {CONCURRENT}/{CONCURRENT_OF}: the \
             panes were served in turn, not together",
            together.each_wanted,
            together.least_at_first_finish
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

/// One row of a Markdown table: its figure, what was measured, its ceiling.
type Row = (String, String, String);

/// Every three-cell row of the Markdown tables in `text`, headers and rules
/// included; a caller looks rows up by figure.
fn rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .filter_map(|inner| {
            let mut cells = inner.split('|').map(|cell| cell.trim().to_owned());
            let row = (cells.next()?, cells.next()?, cells.next()?);
            cells.next().is_none().then_some(row)
        })
        .collect()
}

/// A cell's number and its unit — `0.099 ms` is `0.099` and `ms`; a floor
/// written `at least 50 MiB/s` is `50` and `MiB/s` — or nothing when the cell
/// is not one number and a unit.
fn quantity(cell: &str) -> Option<(f64, &str)> {
    let (number, unit) = cell
        .strip_prefix("at least ")
        .unwrap_or(cell)
        .split_once(' ')?;
    Some((number.parse().ok()?, unit))
}

/// What is wrong with one committed row against the row the benchmark would
/// print: a different ceiling, a measured value that is not a number in the
/// benchmark's unit, or one on the wrong side of its own ceiling.
fn wrong_with(committed: &Row, printed: &Row) -> Option<String> {
    let (named, measured, ceiling) = committed;
    if *ceiling != printed.2 {
        return Some(format!(
            "`{named}`: the ceiling is `{ceiling}`, the benchmark's is `{}`",
            printed.2
        ));
    }
    let Some((value, unit)) = quantity(measured) else {
        return Some(format!("`{named}`: `{measured}` is not a measurement"));
    };
    let wanted_unit = quantity(&printed.1).map(|(_zero, printed_unit)| printed_unit);
    if wanted_unit != Some(unit) {
        return Some(format!("`{named}`: `{measured}` is not in {wanted_unit:?}"));
    }
    let bound = quantity(ceiling).filter(|(_bound, bound_unit)| *bound_unit == unit);
    let within = bound.is_none_or(|(bound, _unit)| {
        if ceiling.starts_with("at least ") {
            value >= bound
        } else {
            value < bound
        }
    });
    (!within).then(|| format!("`{named}`: `{measured}` is outside `{ceiling}`"))
}

/// # Panics
///
/// When the committed table does not carry every figure the benchmark
/// reports, which is how a number in the notes goes stale unnoticed; when a
/// committed ceiling differs from the one the code asserts; when a committed
/// figure is not a number in the benchmark's unit, or is outside its own
/// ceiling; or when the committed aggregate throughput is under [`SHARED`]
/// tenths of the committed single-pane figure.
#[ignore = "reads the committed table; run with the rest of the baseline"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_notes_carry_every_figure() {
    let case = async {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(NOTES);
        let committed = rows(&std::fs::read_to_string(&path)?);
        let printed = rows(&table(&Figures {
            idle_middle: Duration::ZERO,
            idle_tail: Duration::ZERO,
            flood_middle: Duration::ZERO,
            flood_tail: Duration::ZERO,
            single_throughput: 0,
            aggregate_throughput: 0,
            resting_memory: 0,
            many_pane_memory: 0,
            startup: Duration::ZERO,
        }));
        let figure = |named: &str| committed.iter().find(|row| row.0 == named);
        let mut wrong = Vec::new();
        for row in printed
            .iter()
            .filter(|row| row.0 != "Figure" && !row.0.starts_with('-'))
        {
            match figure(&row.0) {
                None => wrong.push(format!("`{}` is missing", row.0)),
                Some(held) => wrong.extend(wrong_with(held, row)),
            }
        }
        let single = figure("One pane's throughput").and_then(|row| quantity(&row.1));
        let together = figure("Eight panes' throughput together").and_then(|row| quantity(&row.1));
        if let (Some((single, _unit)), Some((together, _same))) = (single, together) {
            let shared = f64::from(u32::try_from(SHARED)?) / f64::from(u32::try_from(SHARED_OF)?);
            if together < single * shared {
                wrong.push(format!(
                    "eight panes' {together} is under {SHARED}/{SHARED_OF} of one pane's {single}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}: {wrong:#?}", path.display());
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
