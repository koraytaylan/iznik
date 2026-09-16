//! The headless snapshot-to-draw-list budget, also included in the grid tests.

#[path = "../tests/support/mod.rs"]
pub(crate) mod support;

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

use iznik_app::grid::draw_list;
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
fn measure(options: &BudgetOptions) -> Result<(), Box<dyn std::error::Error>> {
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
