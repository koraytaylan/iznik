//! A selection names retained rows, so the viewport moving does not move it.

use iznik_app::grid::{GridPosition, GridSelection, RetainedPosition, RetainedSelection};
use iznik_app::vt::Viewport;

/// A viewport of `rows` rows whose top is retained row `offset`.
fn viewport(offset: u64, rows: u64) -> Viewport {
    Viewport {
        total: offset.saturating_add(rows),
        offset,
        rows,
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
