//! How many rows the emulator has let go of from the top of its history.
//!
//! The emulator numbers its rows from the oldest one it still holds, so once
//! the history is full every row's number drops each time the oldest go. A
//! selection kept by number would then slide onto other text. Counting the
//! rows let go lets a selection be kept by a number that does not move — the
//! count plus the emulator's own — and be dropped once its rows are gone.
//!
//! The count comes from the emulator itself: a tracked reference on the last
//! row it holds. Nothing above that row can be added to, so the only thing
//! that lowers its number is rows above it going. When the marked row itself
//! has gone, how many went with it is not known, so every row held before is
//! counted as gone, which drops every selection that could have been on them.

use libghostty_vt::Terminal;
use libghostty_vt::screen::{Screen, TrackedGridRef};
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace};

use super::VtError;

/// The marked row and what has been counted so far.
#[derive(Default)]
pub(super) struct Eviction {
    /// A reference that follows the last row held when it was set.
    marker: Option<TrackedGridRef>,
    /// That row's number when it was last read.
    marked_row: u64,
    /// The screen the marker is on; a switch between screens starts again.
    screen: Option<Screen>,
    /// Rows let go of since the emulator began.
    evicted: u64,
}

impl core::fmt::Debug for Eviction {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Eviction")
            .field("marked_row", &self.marked_row)
            .field("evicted", &self.evicted)
            .finish_non_exhaustive()
    }
}

impl Eviction {
    /// Rows let go of as of the last [`Eviction::observe`].
    pub(super) fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Count what went since the last look, and mark the last row again.
    ///
    /// # Errors
    /// Propagates emulator reads and a marker the emulator will not set.
    pub(super) fn observe(&mut self, terminal: &mut Terminal<'_, '_>) -> Result<u64, VtError> {
        let total = terminal.scrollbar()?.total;
        let screen = terminal.active_screen()?;
        if let Some(marker) = self.marker.as_ref() {
            let went = if self.screen == Some(screen) {
                match marker.point(PointSpace::Screen)? {
                    Some(point) => self.marked_row.saturating_sub(u64::from(point.y)),
                    None => self.marked_row.saturating_add(total).saturating_add(1),
                }
            } else {
                self.marked_row.saturating_add(total).saturating_add(1)
            };
            self.evicted = self.evicted.saturating_add(went);
        }
        let last = total.saturating_sub(1);
        let point = Point::Screen(PointCoordinate {
            x: 0,
            y: u32::try_from(last).map_err(|_many| VtError::Overflow)?,
        });
        match self.marker.as_mut() {
            Some(marker) => {
                let _moved = marker.set(terminal, point)?;
            }
            None => self.marker = Some(terminal.track_grid_ref(point)?),
        }
        self.marked_row = last;
        self.screen = Some(screen);
        Ok(self.evicted)
    }
}
