//! Terminal geometry and row invalidation over owned emulator snapshots.
//! The renderer never parses terminal bytes or reconstructs history.

mod draw;
mod ime;
mod interaction;
pub mod keyboard;
pub mod link;
mod paint;
mod selection;
mod shapes;
pub use paint::align_glyphs;
pub(crate) use paint::color as terminal_color;

use iznik_client::host::manager::credit::CreditReceipt;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::{
    App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render, ScrollDelta,
    SharedString, Styled, TestSupportExt, TextSystem, Window, div, font, px,
};
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::style::{RgbColor, Style};
use libghostty_vt::terminal::ScrollViewport;

use iznik_protocol::identity::Sequence;

use crate::input::{KeyInput, TerminalInput};
use crate::vt::{PaneKey, TerminalSnapshot};
use draw::{braille_mask, is_braille_run, is_drawn_run, is_symbol_text};
pub use draw::{changed_rows, draw_list};
use paint::RowView;
pub use selection::{RetainedPosition, RetainedSelection};
pub use shapes::{CellShape, cell_shapes};

/// Initial font size in logical pixels; settings can replace it.
const FONT_SIZE: f32 = 14.0;
/// Initial cell height leaves room for accents and underline decoration.
const LINE_HEIGHT: f32 = 18.0;
/// Initial cell width before the configured font is measured by GPUI.
const CELL_WIDTH: f32 = 8.0;
/// Number of terminal columns occupied by a wide grapheme.
const WIDE_COLUMNS: u16 = 2;

/// Font and cell geometry used by layout, drawing, selection and IME.
#[derive(Clone, Debug, PartialEq)]
pub struct GridMetrics {
    /// Configured monospaced font family, including ligature features GPUI enables.
    pub font: SharedString,
    /// Font size in logical pixels.
    pub font_size: Pixels,
    /// Width of one terminal column.
    pub cell_width: Pixels,
    /// Height of one terminal row.
    pub line_height: Pixels,
}

impl Default for GridMetrics {
    fn default() -> Self {
        Self {
            font: "monospace".into(),
            font_size: px(FONT_SIZE),
            cell_width: px(CELL_WIDTH),
            line_height: px(LINE_HEIGHT),
        }
    }
}

/// A position within the displayed viewport, ordered in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPosition {
    /// Zero-based row.
    pub row: u16,
    /// Zero-based column; an end position may be just past the row.
    pub column: u16,
}

/// Half-open selection endpoints; reversed dragging is normalized on use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSelection {
    /// Where the drag began.
    pub anchor: GridPosition,
    /// Current drag endpoint, excluded from selection.
    pub head: GridPosition,
}

impl GridSelection {
    /// Columns selected on a row, clipped to its width.
    #[must_use]
    pub fn row(self, row: u16, columns: u16) -> Option<Range<u16>> {
        let start = self.anchor.min(self.head);
        let end = self.anchor.max(self.head);
        if row < start.row || row > end.row {
            return None;
        }
        let left = if row == start.row {
            start.column.min(columns)
        } else {
            0
        };
        let right = if row == end.row {
            end.column.min(columns)
        } else {
            columns
        };
        (left < right).then_some(left..right)
    }
}

/// A consecutive group of equal-style narrow cells, or one wide grapheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellRun {
    /// Starting terminal column; following runs never depend on glyph advance.
    pub column: u16,
    /// Number of occupied terminal columns.
    pub columns: u16,
    /// Text passed to GPUI shaping as a single run, retaining ligatures.
    pub text: String,
    /// Byte offset in `text` where each cell's grapheme begins, in column
    /// order; painting puts every cell's glyphs at that cell's column.
    pub starts: Vec<usize>,
    /// Complete emulator style, including decorations and inverse video.
    pub style: Style,
    /// Effective foreground after palette resolution.
    pub foreground: RgbColor,
    /// Effective background after palette resolution.
    pub background: RgbColor,
    /// Underline color after palette resolution and inverse-aware defaulting.
    pub underline: RgbColor,
    /// OSC 8 target shared by every cell in the run. Absent when the run is plain text.
    pub link: Option<String>,
}

/// Cursor geometry in one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorDrawing {
    /// Starting column, corrected from a wide-cell tail when needed.
    pub column: u16,
    /// Width in terminal cells.
    pub columns: u16,
    /// Requested block, bar, or underline shape.
    pub style: CursorVisualStyle,
    /// Effective cursor color.
    pub color: RgbColor,
    /// Whether the pane has keyboard focus: a focused block fills its cell,
    /// and an unfocused cursor of any shape is an outline.
    pub focused: bool,
}

/// Draw list for a single row; equality is the invalidation rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowDrawing {
    /// Width in cells, including blanks.
    pub columns: u16,
    /// Default background behind all cell runs.
    pub background: RgbColor,
    /// Consecutive text runs; wide continuations do not duplicate graphemes.
    pub runs: Vec<CellRun>,
    /// Visible cursor in this row, if any.
    pub cursor: Option<CursorDrawing>,
    /// Half-open selected columns in this row.
    pub selection: Option<Range<u16>>,
}

/// Invalid snapshot geometry, pane routing or stream-credit accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    /// Dimensions or row widths do not describe a rectangular terminal.
    Geometry,
    /// A stale sequence or credit count cannot be consumed without corrupting flow control.
    Credit,
    /// A different host or pane was routed to this existing grid.
    Pane,
}

impl core::fmt::Display for GridError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Geometry => "invalid terminal grid geometry",
            Self::Credit => "invalid terminal credit sequence or count",
            Self::Pane => "snapshot belongs to a different pane",
        })
    }
}
impl core::error::Error for GridError {}

/// A viewport request the window routes to its VT owner.
#[derive(Clone, Debug)]
pub struct GridScroll {
    /// Pane whose history should move.
    pub key: PaneKey,
    /// Emulator-native movement; the renderer holds no history copy.
    pub scroll: ScrollViewport,
}

/// Owned input the window routes to the pane's VT owner for mode-aware encoding.
#[derive(Clone, Debug)]
pub struct GridInput {
    /// Pane that owns the focused terminal surface.
    pub key: PaneKey,
    /// Input data without any terminal handle or mode assumptions.
    pub input: TerminalInput,
}

/// Whether a grid's pane has focus, and how its Option key is read.
#[derive(Clone, Copy, Debug, Default)]
struct PaneState {
    /// Whether the pane has keyboard focus, which shapes its cursor.
    focused: bool,
    /// Whether Option is sent as Meta rather than used by the keyboard layout.
    option_as_meta: bool,
}

/// Visible terminal rows, each a separately cached GPUI paint subtree.
#[derive(Debug)]
pub struct TerminalGrid {
    /// Latest source snapshot, including its sequence and viewport metadata.
    snapshot: Option<TerminalSnapshot>,
    /// Cached entities retain unchanged rows across sibling/chrome redraws.
    rows: Vec<Entity<RowView>>,
    /// Shared geometry for all rows and future IME/selection hit testing.
    metrics: GridMetrics,
    /// Selection stays local to this surface, anchored to retained rows so
    /// output that scrolls the viewport does not move or cancel it.
    selection: Option<RetainedSelection>,
    /// Keyboard focus used by viewport navigation.
    focus: FocusHandle,
    /// Last accepted byte position used to avoid crediting duplicate snapshots.
    consumed_sequence: Option<Sequence>,
    /// Accepted stream bytes whose credit has not yet been submitted to the engine.
    pending_credit: u32,
    /// Accepted delivery identities retained until the engine accepts their return.
    pending_receipts: VecDeque<CreditReceipt>,
    /// Fractional wheel movement retained between input events.
    wheel: Pixels,
    /// Unsent platform composition and its last painted geometry.
    composition: ime::Composition,
    /// Key presses sent to the process, retaining identity for their matching releases.
    pressed_keys: BTreeMap<String, KeyInput>,
    /// Most recent laid-out surface, shared by pointer and composition geometry.
    bounds: Option<Bounds<Pixels>>,
    /// Cell where the currently accepted local selection drag began.
    selection_anchor: Option<RetainedPosition>,
    /// Button whose press began on this surface, including drags outside its bounds.
    pointer_pressed: Option<MouseButton>,
    /// Shift at press time explicitly bypasses program mouse tracking for this drag.
    pointer_override: bool,
    /// Last local gesture failure, included in surface diagnostics.
    interaction_error: Option<String>,
    /// Selection the rows were last drawn with.
    drawn_selection: Option<GridSelection>,
    /// Focus and keyboard choices that shape the cursor and key encoding.
    pane_state: PaneState,
    /// A refused snapshot left rows behind what the emulator has; the next
    /// accepted one draws every row again rather than only its dirty ones.
    stale: bool,
}

impl EventEmitter<GridScroll> for TerminalGrid {}
impl EventEmitter<GridInput> for TerminalGrid {}
impl Focusable for TerminalGrid {
    fn focus_handle(&self, _context: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalGrid {
    /// Construct an empty grid; the first authoritative snapshot supplies size.
    #[must_use]
    pub fn new(metrics: GridMetrics, context: &mut Context<'_, Self>) -> Self {
        Self {
            snapshot: None,
            rows: Vec::new(),
            metrics,
            selection: None,
            focus: context.focus_handle(),
            wheel: px(0.0),
            consumed_sequence: None,
            pending_credit: 0,
            pending_receipts: VecDeque::new(),
            composition: ime::Composition::default(),
            pressed_keys: BTreeMap::new(),
            bounds: None,
            selection_anchor: None,
            pointer_pressed: None,
            pointer_override: false,
            interaction_error: None,
            drawn_selection: None,
            pane_state: PaneState::default(),
            stale: false,
        }
    }

    /// Record whether this pane has keyboard focus, redrawing its cursor row
    /// when that changes.
    ///
    /// # Errors
    /// Returns `Geometry` if the held snapshot is invalid.
    pub fn set_focused(
        &mut self,
        focused: bool,
        context: &mut Context<'_, Self>,
    ) -> Result<usize, GridError> {
        if self.pane_state.focused == focused {
            return Ok(0);
        }
        self.pane_state.focused = focused;
        self.stale = true;
        match self.snapshot.clone() {
            Some(snapshot) => self.apply(snapshot, context),
            None => Ok(0),
        }
    }

    /// Send Option as Meta, or leave it to the keyboard layout.
    pub fn set_option_as_meta(&mut self, option_as_meta: bool) {
        self.pane_state.option_as_meta = option_as_meta;
    }

    /// Replace font and cell geometry, notifying every retained row so it
    /// repaints at the new size instead of waiting for its content to change.
    pub fn set_metrics(&mut self, metrics: &GridMetrics, context: &mut Context<'_, Self>) {
        self.metrics = metrics.clone();
        for row in &self.rows {
            row.update(context, |row, context| {
                row.set_metrics(metrics);
                context.notify();
            });
        }
    }

    /// Apply a snapshot and notify only rows whose visible drawing changed.
    /// Returns the changed-row count and queues credit only for newly accepted bytes.
    /// The window flushes that credit through the engine after successful consumption.
    ///
    /// # Errors
    /// Rejects invalid geometry, a different pane or invalid credit accounting without
    /// changing this surface.
    pub fn apply(
        &mut self,
        snapshot: TerminalSnapshot,
        context: &mut Context<'_, Self>,
    ) -> Result<usize, GridError> {
        let applied = self.apply_rows(snapshot, context);
        self.stale = applied.is_err();
        applied
    }

    /// Apply a snapshot whose geometry and credit have not been checked yet.
    ///
    /// # Errors
    /// As [`Self::apply`].
    fn apply_rows(
        &mut self,
        mut snapshot: TerminalSnapshot,
        context: &mut Context<'_, Self>,
    ) -> Result<usize, GridError> {
        if ![
            self.metrics.font_size,
            self.metrics.cell_width,
            self.metrics.line_height,
        ]
        .into_iter()
        .all(|value| f32::from(value).is_finite() && value > px(0.0))
            || self.metrics.font.is_empty()
        {
            return Err(GridError::Geometry);
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|held| held.key != snapshot.key)
        {
            return Err(GridError::Pane);
        }
        let previous = self.snapshot.as_ref().filter(|_| !self.stale);
        let evicted = self
            .selection
            .is_some_and(|selected| selected.evicted(snapshot.viewport))
            || self
                .selection_anchor
                .is_some_and(|anchor| anchor.row < snapshot.viewport.evicted);
        if snapshot.reset || evicted {
            // A new emulator numbers its rows afresh, and text the history
            // let go of is not there to select: either way no row the
            // selection names holds what it held.
            self.selection = None;
            self.selection_anchor = None;
        }
        let shown = self
            .selection
            .and_then(|selected| selected.visible(snapshot.viewport, snapshot.columns));
        let drawings = changed_rows(
            &snapshot,
            previous,
            self.drawn_selection,
            shown,
            self.pane_state.focused,
        )?;
        let (pending_credit, received) = self.credit_after(&snapshot)?;
        let mut changed = 0_usize;
        let resized = snapshot.rows.len() != self.rows.len();
        for (index, drawing) in drawings {
            if let Some(row) = self.rows.get(index) {
                let changed_row = row.read(context).drawing.as_ref() != &drawing;
                if changed_row {
                    row.update(context, |row, context| {
                        row.drawing = Arc::new(drawing);
                        context.notify();
                    });
                    changed = changed.saturating_add(1);
                }
            } else {
                self.rows
                    .push(context.new(|_context| RowView::new(drawing, self.metrics.clone())));
                changed = changed.saturating_add(1);
            }
        }
        self.rows.truncate(snapshot.rows.len());
        self.drawn_selection = shown;
        self.pending_credit = pending_credit;
        let receipts = std::mem::take(&mut snapshot.receipts);
        if received {
            self.pending_receipts.extend(receipts);
        }
        self.consumed_sequence = Some(snapshot.sequence);
        // Local overlays reuse this frame but cannot repeat its credit or reset.
        snapshot.consumed_bytes = 0;
        snapshot.reset = false;
        self.snapshot = Some(snapshot);
        if changed != 0 || resized {
            context.notify();
        }
        Ok(changed)
    }

    /// Determine credit without mutating the surface if geometry or accounting fails.
    ///
    /// # Errors
    /// Rejects stale ordinary frames, impossible byte ranges and pending-credit overflow.
    fn credit_after(&self, snapshot: &TerminalSnapshot) -> Result<(u32, bool), GridError> {
        if snapshot.reset {
            return if snapshot.consumed_bytes == 0 && snapshot.receipts.is_empty() {
                Ok((self.pending_credit, false))
            } else {
                Err(GridError::Credit)
            };
        }
        let start = snapshot
            .sequence
            .0
            .checked_sub(u64::from(snapshot.consumed_bytes))
            .ok_or(GridError::Credit)?;
        let previous = self.consumed_sequence.map_or(start, |sequence| sequence.0);
        let remaining = snapshot
            .sequence
            .0
            .checked_sub(previous)
            .ok_or(GridError::Credit)?;
        let accepted = remaining.min(u64::from(snapshot.consumed_bytes));
        if !snapshot.receipts.is_empty() {
            let receipt_bytes = snapshot
                .receipts
                .iter()
                .try_fold(0_u64, |total, receipt| {
                    (receipt.host() == &snapshot.key.host && receipt.pane() == snapshot.key.pane)
                        .then(|| total.checked_add(u64::from(receipt.bytes())))
                        .flatten()
                })
                .ok_or(GridError::Credit)?;
            if receipt_bytes != u64::from(snapshot.consumed_bytes)
                || (accepted != 0 && accepted != receipt_bytes)
            {
                return Err(GridError::Credit);
            }
            return Ok((self.pending_credit, accepted != 0));
        }
        self.pending_credit
            .checked_add(u32::try_from(accepted).map_err(|_large| GridError::Credit)?)
            .map(|pending| (pending, false))
            .ok_or(GridError::Credit)
    }

    /// Submit accepted bytes as one pane-scoped credit grant, retaining them on failure.
    /// The callback must return success only after the grant is accepted by its transport.
    /// Zero-credit frames, duplicate frames and local selection changes submit nothing.
    ///
    /// # Errors
    /// Returns the callback's failure without clearing the pending grant.
    pub fn flush_credit<Failure>(
        &mut self,
        submit: impl FnOnce(&PaneKey, u32) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        if self.pending_credit != 0
            && let Some(snapshot) = &self.snapshot
        {
            submit(&snapshot.key, self.pending_credit)?;
            self.pending_credit = 0;
        }
        Ok(())
    }

    /// Return accepted delivery receipts in order, retaining each failed submission.
    /// Successful earlier submissions are removed even if a later one fails.
    ///
    /// # Errors
    /// Returns the callback's failure without removing the receipt it rejected.
    pub fn flush_receipts<Failure>(
        &mut self,
        mut submit: impl FnMut(&CreditReceipt) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        while let Some(receipt) = self.pending_receipts.front() {
            submit(receipt)?;
            self.pending_receipts.pop_front();
        }
        Ok(())
    }

    /// Set local selection over the displayed viewport and invalidate only
    /// rows whose overlay changes.
    ///
    /// # Errors
    /// Returns `Geometry` if the held snapshot is invalid.
    pub fn select(
        &mut self,
        selection: Option<GridSelection>,
        context: &mut Context<'_, Self>,
    ) -> Result<usize, GridError> {
        let viewport = self.snapshot.as_ref().map(|held| held.viewport);
        let retained = selection
            .zip(viewport)
            .map(|(selected, shown)| RetainedSelection::from_viewport(selected, shown));
        self.select_retained(retained, context)
    }

    /// Set local selection over retained rows.
    ///
    /// # Errors
    /// Returns `Geometry` if the held snapshot is invalid.
    pub fn select_retained(
        &mut self,
        selection: Option<RetainedSelection>,
        context: &mut Context<'_, Self>,
    ) -> Result<usize, GridError> {
        self.selection = selection;
        match self.snapshot.clone() {
            Some(snapshot) => self.apply(snapshot, context),
            None => Ok(0),
        }
    }

    /// The current selection, over retained rows.
    #[must_use]
    pub fn selection(&self) -> Option<RetainedSelection> {
        self.selection
    }

    /// Snapshot whose sequence and viewport the surface currently displays.
    #[must_use]
    pub fn snapshot(&self) -> Option<&TerminalSnapshot> {
        self.snapshot.as_ref()
    }

    /// Font and cell geometry this grid currently paints with.
    #[must_use]
    pub fn metrics(&self) -> &GridMetrics {
        &self.metrics
    }

    /// Paint calls, per row, for diagnostics and cache verification.
    #[must_use]
    pub fn paint_counts(&self, context: &App) -> Vec<u64> {
        self.rows
            .iter()
            .map(|row| row.read(context).paint_count.get())
            .collect()
    }

    /// Most recent painting or local gesture failures for window diagnostics.
    #[must_use]
    pub fn paint_errors(&self, context: &App) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|row| row.read(context).error.borrow().clone())
            .chain(self.composition.error.clone())
            .chain(self.interaction_error.clone())
            .collect()
    }

    /// Emit a scroll request; only the VT owner changes the displayed viewport.
    pub fn scroll(&self, scroll: ScrollViewport, context: &mut Context<'_, Self>) {
        if let Some(snapshot) = &self.snapshot {
            context.emit(GridScroll {
                key: snapshot.key.clone(),
                scroll,
            });
        }
    }

    /// Accumulate smooth pixel deltas and emit complete rows only.
    fn wheel_rows(&mut self, delta: ScrollDelta) -> Option<isize> {
        let delta_pixels = match delta {
            ScrollDelta::Pixels(point) => point.y,
            ScrollDelta::Lines(point) => scale(self.metrics.line_height, point.y),
        };
        self.wheel = offset(self.wheel, delta_pixels);
        if self.metrics.line_height <= px(0.0) {
            return None;
        }
        let count = usize::from(px(
            f32::from(self.wheel.abs()) / f32::from(self.metrics.line_height)
        ));
        if count == 0 {
            return None;
        }
        let direction = if self.wheel > px(0.0) { 1 } else { -1 };
        let rows = isize::try_from(count).unwrap_or(isize::MAX);
        let consumed = scale(self.metrics.line_height, f32::from(Pixels::from(count)));
        self.wheel = if direction > 0 {
            distance(self.wheel, consumed)
        } else {
            offset(self.wheel, consumed)
        };
        Some(if direction > 0 {
            rows.saturating_neg()
        } else {
            rows
        })
    }
}

impl Render for TerminalGrid {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let columns = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.columns);
        let width = scale(self.metrics.cell_width, f32::from(columns));
        let mut style = div().w(width).h(self.metrics.line_height).flex_shrink_0();
        let style = style.style().clone();
        let mut surface = div()
            .id("terminal-grid")
            .test_support()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .flex()
            .flex_col()
            .relative()
            .w(width)
            .overflow_hidden()
            .on_scroll_wheel(context.listener(
                |grid, event: &gpui_kit::ScrollWheelEvent, window, context| {
                    grid.pointer_wheel(event, window, context);
                },
            ))
            .on_key_down(context.listener(|grid, event, _event_window, context| {
                grid.key_down(event, context);
            }))
            .on_key_up(context.listener(|grid, event, _event_window, context| {
                grid.key_up(event, context);
            }))
            .on_action(
                context.listener(|grid, _: &crate::menu::Copy, _action_window, context| {
                    grid.copy_selection(context);
                }),
            )
            .on_action(
                context.listener(|grid, _: &crate::menu::Paste, _action_window, context| {
                    if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                        grid.paste_text(text, context);
                    }
                }),
            )
            .children(
                self.rows
                    .iter()
                    .map(|row| row.clone().cached(style.clone())),
            )
            .child(ime::overlay(context.entity()))
            .on_mouse_move(context.listener(|grid, event, window, context| {
                grid.pointer_move(event, window, context);
            }));
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            surface = surface
                .on_mouse_down(
                    button,
                    context.listener(|grid, event, window, context| {
                        grid.pointer_down(event, window, context);
                    }),
                )
                .on_mouse_up(
                    button,
                    context.listener(|grid, event, window, context| {
                        grid.pointer_up(event, window, context);
                    }),
                )
                .on_mouse_up_out(
                    button,
                    context.listener(|grid, event, window, context| {
                        grid.pointer_up(event, window, context);
                    }),
                );
        }
        surface
    }
}

/// Convert floating-point geometry into finite pixels, saturating overflow and
/// treating undefined geometry as zero so it never reaches the GPU renderer.
fn finite(value: f32) -> Pixels {
    px(if value.is_nan() {
        0.0
    } else {
        value.clamp(-f32::MAX, f32::MAX)
    })
}

/// Add logical coordinates with explicit finite saturation.
fn offset(left: Pixels, right: Pixels) -> Pixels {
    finite(f32::from(left) + f32::from(right))
}

/// Subtract logical coordinates with explicit finite saturation.
fn distance(left: Pixels, right: Pixels) -> Pixels {
    finite(f32::from(left) - f32::from(right))
}

/// Scale cell geometry with explicit finite saturation.
fn scale(value: Pixels, factor: f32) -> Pixels {
    finite(f32::from(value) * factor)
}

/// Measure one monospace cell's pixel geometry for a font, size and line
/// spacing, so a changed font actually changes what's painted instead of being
/// clipped to the previous font's dimensions. Falls back to the initial cell
/// width when the font has no "0" glyph to measure.
///
/// A row is `line_spacing` times the font size, rounded to whole pixels so
/// rows never blur, and never shorter than the font's own ascent and descent,
/// which is what glyphs need to not overlap. GPUI centres each line's glyphs
/// in the row, so the spacing is shared above and below.
pub(crate) fn measure_cell(
    text_system: &TextSystem,
    family: SharedString,
    font_size: Pixels,
    line_spacing: f32,
) -> (Pixels, Pixels) {
    let font_id = text_system.resolve_font(&font(family));
    let cell_width = text_system
        .ch_advance(font_id, font_size)
        .unwrap_or(px(CELL_WIDTH));
    let glyph_height = offset(
        text_system.ascent(font_id, font_size),
        text_system.descent(font_id, font_size),
    );
    let spaced = scale(font_size, line_spacing).round();
    (cell_width, spaced.max(glyph_height.ceil()))
}

/// Convert positive finite surface geometry to complete cells within the protocol range.
pub(crate) fn cells(extent: Pixels, cell: Pixels) -> Option<u16> {
    let extent = f32::from(extent);
    let cell = f32::from(cell);
    if !extent.is_finite() || !cell.is_finite() || extent <= 0.0 || cell <= 0.0 {
        return None;
    }
    let count = extent.div_euclid(cell).clamp(1.0, f32::from(u16::MAX));
    u16::try_from(usize::from(px(count))).ok()
}
