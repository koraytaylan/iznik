//! The headless grid budgets, also included in the grid tests: converting one
//! full snapshot to draw lists, and a flood of output from emulator feeding
//! through snapshot extraction to the grid's row rebuild.

#[path = "../tests/support/mod.rs"]
pub(crate) mod support;

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

/// Fixture and measurement failures.
type Failed = Box<dyn std::error::Error>;

use iznik_app::grid::{changed_rows, draw_list};
use iznik_app::vt::{VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;

/// One hundred columns by one hundred rows is the planned ten-thousand-cell grid.
const COLUMNS: u16 = 100;
/// Visible rows in the budget fixture.
const ROWS: u16 = 100;
/// Extra rows ensure the snapshot was produced after real emulator scrolling.
const OUTPUT_ROWS: u16 = 120;
/// Alternate ANSI colors per cell to exercise the maximum run count.
const PALETTE_COLORS: u16 = 16;
/// Short CPU-only samples, enough to report a worst case without a timing window.
const SAMPLES: usize = 32;
/// Debug-build CPU draw-list ceiling; excludes native shaping and GPU frame time.
/// Twenty milliseconds catches a substantial mapping regression without claiming
/// a display refresh rate from a headless test.
const DRAW_CEILING: Duration = Duration::from_millis(20);

/// Chunks in one flood, each delivered as its own `Feed` like transport frames.
const CHUNKS: u16 = 256;
/// Rows of output each chunk writes, so the flood scrolls the grid many times.
const CHUNK_ROWS: u16 = 4;
/// Independent floods, enough to report a worst case without a timing window.
const FLOOD_SAMPLES: usize = 4;
/// Debug-build ceiling for one flood: 102,400 colored cells of output fed,
/// every snapshot the thread publishes extracted, and every changed row drawn
/// again as `TerminalGrid::apply` does. Measured at 17 ms on an Apple M-series
/// laptop, where one full snapshot and one full redraw per chunk took 0.8 s;
/// the ceiling leaves ten times the measurement for a loaded
/// gate machine and still catches that regression.
const FLOOD_CEILING: Duration = Duration::from_millis(200);

/// Measurement controls are values so tests do not wait on product constants.
struct BudgetOptions {
    /// Number of independent conversions, not a wall-clock loop.
    samples: usize,
    /// Maximum time allowed for any one conversion.
    ceiling: Duration,
}

impl Default for BudgetOptions {
    fn default() -> Self {
        Self {
            samples: SAMPLES,
            ceiling: DRAW_CEILING,
        }
    }
}

/// Build colored rows as input to the real VT owner, not synthetic cell structs.
fn output() -> Vec<u8> {
    let mut bytes = String::new();
    for row in 0..OUTPUT_ROWS {
        if row != 0 {
            bytes.push_str("\r\n");
        }
        for column in 0..COLUMNS {
            let color = row.saturating_add(column).wrapping_rem(PALETTE_COLORS);
            let _written = write!(bytes, "\x1b[38;5;{color}mX");
        }
    }
    bytes.into_bytes()
}

/// Measure only the conversion the grid's apply path uses, with emulator work
/// outside the timed region. Returns a table suitable for the committed note.
///
/// # Errors
/// Propagates thread, emulator, draw-list, or standard-output failures.
///
/// # Panics
/// Fails if the fixture is not a scrolling ten-thousand-cell grid or any
/// conversion exceeds its committed ceiling.
fn measure(options: &BudgetOptions) -> Result<(), Failed> {
    let thread = VtThread::start(VtOptions::default())?;
    support::open(&thread, Sequence(0), COLUMNS, ROWS)?;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: support::key(),
        sequence: Sequence(0),
        bytes: output(),
    })?;
    let current = support::snapshot(&thread)?;
    assert!(
        current.scrollback_rows != 0,
        "the fixture must have scrolled"
    );
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    for _ in 0..options.samples {
        let started = Instant::now();
        let rows = std::hint::black_box(draw_list(std::hint::black_box(&current), None)?);
        let elapsed = started.elapsed();
        worst = worst.max(elapsed);
        total = total.saturating_add(elapsed);
        assert_eq!(rows.len(), usize::from(ROWS), "visible row count");
        assert!(
            rows.iter().all(|row| row.columns == COLUMNS),
            "visible column count"
        );
        assert!(
            elapsed < options.ceiling,
            "draw list took {elapsed:?}, ceiling {:?}",
            options.ceiling
        );
    }
    let samples = u32::try_from(options.samples)?;
    let average = total
        .checked_div(samples)
        .ok_or("budget needs at least one sample")?;
    let table = format!(
        "| Workload | Samples | Average | Worst | Ceiling |\n|---|---:|---:|---:|---:|\n| {COLUMNS}x{ROWS} scrolling cells, alternating colors | {samples} | {average:?} | {worst:?} | {:?} |\n",
        options.ceiling
    );
    std::io::stdout().lock().write_all(table.as_bytes())?;
    Ok(())
}

/// The draw-list implementation stays within its committed headless ceiling.
///
/// # Panics
/// Fails on fixture, conversion or budget failures.
#[test]
fn grid_draw_list_stays_within_its_budget() {
    measure(&BudgetOptions::default()).expect("headless grid budget");
}

/// One chunk of colored rows, as the real VT owner receives output.
fn chunk(index: u16) -> Vec<u8> {
    let mut bytes = String::new();
    for row in 0..CHUNK_ROWS {
        bytes.push_str("\r\n");
        for column in 0..COLUMNS {
            let color = index
                .wrapping_add(row)
                .wrapping_add(column)
                .wrapping_rem(PALETTE_COLORS);
            let _written = write!(bytes, "\x1b[38;5;{color}mX");
        }
    }
    bytes.into_bytes()
}

/// Feed one flood and consume every snapshot it produces the way the grid
/// does: rows drawn again only where they changed. Returns the elapsed time
/// and how many snapshots the flood's chunks were coalesced into.
///
/// # Errors
/// Propagates thread, emulator, draw-list and sequence failures.
///
/// # Panics
/// Fails when a snapshot has more rows than the grid, or a reply misses its
/// deadline.
fn flood(thread: &VtThread, start: Sequence) -> Result<(Duration, usize, Sequence), Failed> {
    let chunks: Vec<Vec<u8>> = (0..CHUNKS).map(chunk).collect();
    let started = Instant::now();
    let mut sequence = start.0;
    for bytes in chunks {
        let length = u64::try_from(bytes.len())?;
        thread.send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(sequence),
            bytes,
        })?;
        sequence = sequence.checked_add(length).ok_or("sequence overflow")?;
    }
    let mut previous = None;
    let mut snapshots = 0_usize;
    loop {
        let current = support::snapshot(thread)?;
        let rows =
            std::hint::black_box(changed_rows(&current, previous.as_ref(), None, None, true)?);
        assert!(rows.len() <= usize::from(ROWS), "visible row count");
        snapshots = snapshots.checked_add(1).ok_or("snapshot count overflow")?;
        let done = current.sequence == Sequence(sequence);
        previous = Some(current);
        if done {
            return Ok((started.elapsed(), snapshots, Sequence(sequence)));
        }
    }
}

/// Measure floods through the real VT owner and the grid's row rebuild, with
/// the ceiling a constant: the claims registry pins the draw-list budget alone.
/// Returns a table suitable for the committed note.
///
/// # Errors
/// Propagates thread, emulator, draw-list, or standard-output failures.
///
/// # Panics
/// Fails if any flood exceeds its committed ceiling.
fn measure_flood() -> Result<(), Failed> {
    let thread = VtThread::start(VtOptions::default())?;
    let mut sequence = Sequence(0);
    support::open(&thread, sequence, COLUMNS, ROWS)?;
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let mut most_snapshots = 0_usize;
    for _ in 0..FLOOD_SAMPLES {
        let (elapsed, snapshots, next) = flood(&thread, sequence)?;
        sequence = next;
        worst = worst.max(elapsed);
        most_snapshots = most_snapshots.max(snapshots);
        total = total.saturating_add(elapsed);
        assert!(
            elapsed < FLOOD_CEILING,
            "flood took {elapsed:?}, ceiling {FLOOD_CEILING:?}"
        );
    }
    let samples = u32::try_from(FLOOD_SAMPLES)?;
    let average = total
        .checked_div(samples)
        .ok_or("budget needs at least one sample")?;
    let table = format!(
        "| Workload | Samples | Average | Worst | Most snapshots | Ceiling |\n|---|---:|---:|---:|---:|---:|\n| {CHUNKS} chunks of {CHUNK_ROWS} rows into {COLUMNS}x{ROWS} cells, alternating colors | {samples} | {average:?} | {worst:?} | {most_snapshots} | {FLOOD_CEILING:?} |\n"
    );
    std::io::stdout().lock().write_all(table.as_bytes())?;
    Ok(())
}

/// A flood of output stays within its committed headless ceiling.
///
/// # Panics
/// Fails on fixture, conversion or budget failures.
#[test]
fn grid_flood_stays_within_its_budget() {
    measure_flood().expect("headless flood budget");
}
