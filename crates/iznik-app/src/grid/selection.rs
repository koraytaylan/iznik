//! Selection anchored to the pane's retained rows rather than to the viewport.
//!
//! A row number counts from the first row the emulator ever held, history and
//! screen together: the emulator's own screen coordinates plus the rows its
//! full history has let go of since. Output that scrolls the viewport
//! therefore moves what a selection covers on screen, not which text it
//! covers, and a drag that scrolls keeps its anchor. Rows let go of do not
//! renumber the rest, so a selection stays on its text until that text itself
//! is let go of, and is then dropped rather than moved onto other rows.

use crate::vt::Viewport;

use super::{GridPosition, GridSelection};

/// A cell among the pane's retained rows, ordered in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetainedPosition {
    /// Row counted from the first row the emulator ever held.
    pub row: u64,
    /// Zero-based column; an end position may be just past the row.
    pub column: u16,
}

impl RetainedPosition {
    /// The retained cell under a viewport cell of a frame showing `viewport`.
    #[must_use]
    pub fn from_viewport(position: GridPosition, viewport: Viewport) -> Self {
        Self {
            row: viewport.top().saturating_add(u64::from(position.row)),
            column: position.column,
        }
    }
}

/// Half-open selection over retained rows; reversed dragging is normalized on use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetainedSelection {
    /// Where the drag began.
    pub anchor: RetainedPosition,
    /// Current drag endpoint, excluded from selection.
    pub head: RetainedPosition,
}

impl RetainedSelection {
    /// A viewport selection made against a frame showing `viewport`.
    #[must_use]
    pub fn from_viewport(selection: GridSelection, viewport: Viewport) -> Self {
        Self {
            anchor: RetainedPosition::from_viewport(selection.anchor, viewport),
            head: RetainedPosition::from_viewport(selection.head, viewport),
        }
    }

    /// The part of this selection a frame showing `viewport` displays, in its
    /// viewport cells; `None` when none of it is on screen.
    #[must_use]
    pub fn visible(self, viewport: Viewport, columns: u16) -> Option<GridSelection> {
        let start = self.anchor.min(self.head);
        let end = self.anchor.max(self.head);
        let rows = viewport.rows.min(u64::from(u16::MAX));
        let top_row = viewport.top();
        let bottom = top_row.checked_add(rows)?;
        if end.row < top_row || start.row >= bottom || start == end {
            return None;
        }
        let top = RetainedPosition {
            row: top_row,
            column: 0,
        };
        let last = RetainedPosition {
            row: bottom.checked_sub(1)?,
            column: columns,
        };
        let start = start.max(top);
        let end = end.min(last);
        let shown = |position: RetainedPosition| {
            Some(GridPosition {
                row: u16::try_from(position.row.checked_sub(top_row)?).ok()?,
                column: position.column,
            })
        };
        (start < end).then_some(GridSelection {
            anchor: shown(start)?,
            head: shown(end)?,
        })
    }

    /// Whether any of the rows this selection covers has been let go of by
    /// a history showing `viewport`.
    #[must_use]
    pub fn evicted(self, viewport: Viewport) -> bool {
        self.anchor.min(self.head).row < viewport.evicted
    }
}
