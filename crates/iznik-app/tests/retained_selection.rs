//! A selection names retained rows, so the viewport moving does not move it.

use iznik_app::grid::{GridPosition, GridSelection, RetainedPosition, RetainedSelection};
use iznik_app::vt::{MINIMUM_SCROLLBACK_BYTES, Viewport, VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;

mod support;

/// Width of the eviction fixture's pane.
const COLUMNS: u16 = 80;
/// Height of the eviction fixture's pane.
const ROWS: u16 = 4;
/// Lines written between two looks at the history.
const LINES_PER_FEED: usize = 256;
/// Far more feeds than it takes the smallest history to let rows go.
const MOST_FEEDS: usize = 400;
/// One short line.
const LINE: &[u8] = b"x\r\n";

/// A viewport of `rows` rows whose top is retained row `offset`.
fn viewport(offset: u64, rows: u64) -> Viewport {
    Viewport {
        total: offset.saturating_add(rows),
        offset,
        rows,
        evicted: 0,
    }
}

/// A selection made on one frame shows lower on a frame scrolled back, and
/// is clipped to what that frame shows.
///
/// # Panics
/// Fails when the selection moves with the viewport or is not clipped.
#[test]
fn selection_stays_on_its_rows_as_the_viewport_moves() {
    let made = RetainedSelection::from_viewport(
        GridSelection {
            anchor: GridPosition { row: 1, column: 2 },
            head: GridPosition { row: 3, column: 5 },
        },
        viewport(10, 4),
    );
    assert_eq!(
        made.anchor,
        RetainedPosition { row: 11, column: 2 },
        "anchored to the retained row"
    );
    assert_eq!(
        made.visible(viewport(12, 4), 8),
        Some(GridSelection {
            anchor: GridPosition { row: 0, column: 0 },
            head: GridPosition { row: 1, column: 5 },
        }),
        "output scrolled two rows: the top of it is clipped"
    );
    assert_eq!(made.visible(viewport(20, 4), 8), None, "scrolled away");
}

/// A drag whose anchor has scrolled off the top still covers every row from
/// it to the pointer.
///
/// # Panics
/// Fails when the rows between are not selected.
#[test]
fn drag_across_a_scroll_covers_the_rows_between() {
    let anchor =
        RetainedPosition::from_viewport(GridPosition { row: 0, column: 0 }, viewport(0, 4));
    let head = RetainedPosition::from_viewport(GridPosition { row: 3, column: 4 }, viewport(6, 4));
    let selection = RetainedSelection { anchor, head };
    let shown = selection.visible(viewport(6, 4), 8).expect("visible");
    assert_eq!(shown.row(0, 8), Some(0..8), "rows above the head are whole");
    assert_eq!(
        shown.row(3, 8),
        Some(0..4),
        "the head's row ends at the head"
    );
}

/// Once a full history lets its oldest rows go, the rows after them keep
/// their numbers, and a selection on the rows let go of is dropped rather
/// than moved onto the text that took their place.
///
/// # Errors
/// Propagates emulator thread failures.
///
/// # Panics
/// Fails when the numbering shifts under eviction, or an evicted selection
/// is still reported as present.
#[test]
fn rows_let_go_of_keep_the_rest_numbered() -> Result<(), Box<dyn std::error::Error>> {
    let thread = VtThread::start(VtOptions {
        scrollback_bytes: MINIMUM_SCROLLBACK_BYTES,
        ..VtOptions::default()
    })?;
    let first = support::open(&thread, Sequence(0), COLUMNS, ROWS)?;
    let selected = RetainedSelection::from_viewport(
        GridSelection {
            anchor: GridPosition { row: 0, column: 0 },
            head: GridPosition { row: 0, column: 1 },
        },
        first.viewport,
    );
    assert!(!selected.evicted(first.viewport), "nothing let go yet");
    let chunk = LINE.repeat(LINES_PER_FEED);
    let length = u64::try_from(chunk.len())?;
    let mut sequence = 0_u64;
    let mut lines = 0_u64;
    let mut latest = first.viewport;
    for _ in 0..MOST_FEEDS {
        thread.send(VtCommand::Feed {
            key: support::key(),
            sequence: Sequence(sequence),
            bytes: chunk.clone(),
            receipt: None,
        })?;
        sequence += length;
        lines += u64::try_from(LINES_PER_FEED)?;
        latest = support::snapshot(&thread)?.viewport;
        // The cursor sits on the row after the last line written, at the
        // bottom of the screen, however many rows the history let go of.
        assert_eq!(
            latest.top() + u64::from(ROWS) - 1,
            lines,
            "the bottom row keeps its number: {latest:?}"
        );
        if latest.evicted > 0 {
            break;
        }
    }
    assert!(
        latest.evicted > 0,
        "the smallest history let rows go: {latest:?}"
    );
    assert!(
        selected.evicted(latest),
        "a selection on a row let go of is gone"
    );
    assert_eq!(
        selected.visible(latest, COLUMNS),
        None,
        "and is not drawn over other text"
    );
    Ok(())
}
