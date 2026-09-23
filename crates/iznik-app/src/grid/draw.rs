//! Row draw lists built from owned snapshot cells: text runs, cursor and
//! selection per row, and which rows a new snapshot actually changes.

use libghostty_vt::render::{Colors, CursorVisualStyle};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, StyleColor};

use super::{CellRun, CursorDrawing, GridError, GridSelection, RowDrawing, WIDE_COLUMNS};
use crate::vt::{CellSnapshot, TerminalSnapshot};

/// Build the row draw lists from owned cell state, without parsing byte streams.
///
/// # Errors
/// Returns `Geometry` for zero dimensions, excessive height or nonrectangular rows.
pub fn draw_list(
    snapshot: &TerminalSnapshot,
    selection: Option<GridSelection>,
) -> Result<Vec<RowDrawing>, GridError> {
    check_geometry(snapshot)?;
    (0..snapshot.rows.len())
        .map(|row| row_drawing(snapshot, row, selection, true))
        .collect()
}

/// The rows of `snapshot` that must be drawn again over the frame drawn from
/// `previous` with `previous_selection`, each with its new drawing.
///
/// Every row is drawn again when there is no previous frame or the frame's
/// shape or colors changed. Otherwise a row is drawn again only when the
/// emulator changed it, the cursor left or entered it, or its part of the
/// selection changed — which is what keeps a line of output from rebuilding
/// the whole grid.
///
/// # Errors
/// Returns `Geometry` for zero dimensions, excessive height or nonrectangular rows.
pub fn changed_rows(
    snapshot: &TerminalSnapshot,
    previous: Option<&TerminalSnapshot>,
    previous_selection: Option<GridSelection>,
    selection: Option<GridSelection>,
    focused: bool,
) -> Result<Vec<(usize, RowDrawing)>, GridError> {
    check_geometry(snapshot)?;
    let every_row = previous.is_none_or(|held| {
        held.columns != snapshot.columns
            || held.rows.len() != snapshot.rows.len()
            || held.colors != snapshot.colors
            || held.cursor_style != snapshot.cursor_style
            || held.dirty_rows.len() != snapshot.dirty_rows.len()
    });
    let cursor_rows = [previous.and_then(|held| held.cursor), snapshot.cursor];
    let mut changed = Vec::new();
    for row in 0..snapshot.rows.len() {
        let index = u16::try_from(row).map_err(|_large| GridError::Geometry)?;
        let redraw = every_row
            || snapshot.dirty_rows.get(row).copied().unwrap_or(true)
            || cursor_rows.iter().flatten().any(|cursor| cursor.y == index)
            || previous_selection.and_then(|selected| selected.row(index, snapshot.columns))
                != selection.and_then(|selected| selected.row(index, snapshot.columns));
        if redraw {
            changed.push((row, row_drawing(snapshot, row, selection, focused)?));
        }
    }
    Ok(changed)
}

/// Refuse a snapshot that does not describe a nonempty grid of `u16` rows.
///
/// # Errors
/// Returns `Geometry` for zero dimensions or excessive height.
fn check_geometry(snapshot: &TerminalSnapshot) -> Result<(), GridError> {
    if snapshot.columns == 0
        || snapshot.rows.is_empty()
        || u16::try_from(snapshot.rows.len()).is_err()
    {
        return Err(GridError::Geometry);
    }
    Ok(())
}

/// One row's drawing.
///
/// # Errors
/// Returns `Geometry` when the row is missing or not `columns` cells wide.
fn row_drawing(
    snapshot: &TerminalSnapshot,
    row: usize,
    selection: Option<GridSelection>,
    focused: bool,
) -> Result<RowDrawing, GridError> {
    let cells = snapshot.rows.get(row).ok_or(GridError::Geometry)?;
    if cells.len() != usize::from(snapshot.columns) {
        return Err(GridError::Geometry);
    }
    let row = u16::try_from(row).map_err(|_large| GridError::Geometry)?;
    let cursor = cursor(snapshot, row, focused);
    let mut runs = cell_runs(cells, &snapshot.colors);
    if let Some(block) = cursor
        .as_ref()
        .filter(|drawn| drawn.focused && drawn.style == CursorVisualStyle::Block)
    {
        runs = under_block(runs, block);
    }
    Ok(RowDrawing {
        columns: snapshot.columns,
        background: snapshot.colors.background,
        runs,
        cursor,
        selection: selection.and_then(|selected| selected.row(row, snapshot.columns)),
    })
}

/// The runs with the cell under a focused block cursor split into a run of
/// its own, filled with the cursor color and drawn in the cell's background,
/// so the character stays legible under the block.
fn under_block(runs: Vec<CellRun>, block: &CursorDrawing) -> Vec<CellRun> {
    let mut drawn = Vec::with_capacity(runs.len());
    for run in runs {
        let end = run.column.saturating_add(run.columns);
        if block.column < run.column || block.column >= end {
            drawn.push(run);
            continue;
        }
        let cell = usize::from(block.column.saturating_sub(run.column));
        let start = run.starts.get(cell).copied().unwrap_or(0);
        let stop = run
            .starts
            .get(cell.saturating_add(1))
            .copied()
            .unwrap_or(run.text.len());
        let piece = |from: usize, to: usize, first: usize, last: usize| {
            let text = run.text.get(from..to)?.to_owned();
            let starts: Vec<usize> = run
                .starts
                .get(first..last)?
                .iter()
                .map(|offset| offset.saturating_sub(from))
                .collect();
            let column = run.column.saturating_add(u16::try_from(first).ok()?);
            let columns = u16::try_from(last.saturating_sub(first)).ok()?;
            (!text.is_empty()).then(|| CellRun {
                column,
                columns,
                text,
                starts,
                ..run.clone()
            })
        };
        let cells = run.starts.len();
        let before = piece(0, start, 0, cell);
        let after = piece(stop, run.text.len(), cell.saturating_add(1), cells);
        let mut under =
            piece(start, stop, cell, cell.saturating_add(1)).unwrap_or_else(|| run.clone());
        if run.starts.len() == 1 {
            under.columns = run.columns;
        }
        under.foreground = if run.style.inverse {
            run.foreground
        } else {
            run.background
        };
        under.background = block.color;
        under.style.inverse = false;
        drawn.extend(before);
        drawn.push(under);
        drawn.extend(after);
    }
    drawn
}

/// Unicode braille patterns occupy U+2800 through U+28FF. The low eight bits
/// of the code point are the dot mask, which the row painter draws itself.
const BRAILLE_BLOCK_ORIGIN: u32 = 0x2800;

/// Geometric shapes and the symbol-and-arrow block. Each is one terminal cell,
/// but fallback fonts give them a smaller advance than the cell, so a run of
/// them collapses into a cluster.
const SYMBOL_SPAN_COUNT: usize = 2;
/// Inclusive code-point spans for [`SYMBOL_SPAN_COUNT`].
const SYMBOL_SPAN: [(u32, u32); SYMBOL_SPAN_COUNT] = [(0x25A0, 0x25FF), (0x2B00, 0x2BFF)];

/// Group narrow cells while isolating wide glyphs, braille patterns and
/// cell symbols so fallback font metrics cannot move the following text off
/// its terminal column.
fn cell_runs(cells: &[CellSnapshot], colors: &Colors) -> Vec<CellRun> {
    let mut runs: Vec<CellRun> = Vec::new();
    let mut previous_wide = false;
    let mut previous_kind = Kind::Text;
    for (column, cell) in cells.iter().enumerate() {
        let Ok(column) = u16::try_from(column) else {
            break;
        };
        if cell.width == CellWide::SpacerTail {
            continue;
        }
        let wide = cell.width == CellWide::Wide;
        let text: String = if cell.text.is_empty() {
            " ".to_owned()
        } else {
            cell.text
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect()
        };
        let kind = kind(&text);
        let same_kind = kind == previous_kind && kind != Kind::Symbol;
        if let Some(last) = runs.last_mut()
            && continues_run(last, cell, wide, previous_wide, same_kind)
        {
            last.starts.push(last.text.len());
            last.text.push_str(&text);
            last.columns = last.columns.saturating_add(1);
        } else {
            runs.push(CellRun {
                column,
                columns: if wide { WIDE_COLUMNS } else { 1 },
                text,
                starts: vec![0],
                style: cell.style,
                foreground: cell.foreground,
                background: cell.background,
                underline: underline_color(cell, colors),
                link: cell.link.clone(),
            });
        }
        previous_wide = wide;
        previous_kind = kind;
    }
    runs
}

/// A narrow cell continues the current run when its style matches and it is
/// the same kind of text. Braille stays in its own runs, and each geometric
/// symbol stays in its own cell, so a chart or a spinner cannot share a shaped
/// line with the text beside it.
fn continues_run(
    run: &CellRun,
    cell: &CellSnapshot,
    wide: bool,
    previous_wide: bool,
    same_kind: bool,
) -> bool {
    !wide
        && !previous_wide
        && same_kind
        && run.style == cell.style
        && run.foreground == cell.foreground
        && run.background == cell.background
        && run.link == cell.link
}

/// How a cell's text is painted, which decides the runs it may share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Shaped by the font with its neighbours.
    Text,
    /// A braille pattern, painted as a dot grid.
    Braille,
    /// Box drawing, a block element or a powerline separator, drawn to the cell.
    Drawn,
    /// A geometric symbol, shaped alone and centered in its cell.
    Symbol,
}

/// The kind of one cell's text.
fn kind(text: &str) -> Kind {
    if is_braille_text(text) {
        Kind::Braille
    } else if is_drawn_run(text) {
        Kind::Drawn
    } else if is_symbol_text(text) {
        Kind::Symbol
    } else {
        Kind::Text
    }
}

/// Whether every scalar in a run is drawn to its cell. An empty run is not.
pub(super) fn is_drawn_run(text: &str) -> bool {
    !text.is_empty() && text.chars().all(super::shapes::is_drawn)
}

/// Whether this cell is exactly one braille pattern.
fn is_braille_text(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|character| braille_mask(character).is_some())
        && characters.next().is_none()
}

/// Dot mask of one braille pattern, or `None` for every other scalar.
pub(super) fn braille_mask(character: char) -> Option<u8> {
    let offset = u32::from(character).checked_sub(BRAILLE_BLOCK_ORIGIN)?;
    u8::try_from(offset).ok()
}

/// Whether this cell is one geometric symbol or arrow whose glyph must stay
/// in its own column. A run of these is how a scanner spinner draws its dots.
pub(super) fn is_symbol_text(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(is_symbol_character) && characters.next().is_none()
}

/// Whether one scalar is a geometric shape or a symbol from the arrow block.
fn is_symbol_character(character: char) -> bool {
    let code = u32::from(character);
    SYMBOL_SPAN
        .iter()
        .any(|range| code >= range.0 && code <= range.1)
}

/// Whether every scalar in a run is a braille pattern. An empty run is not.
pub(super) fn is_braille_run(text: &str) -> bool {
    let mut found = false;
    for character in text.chars() {
        if braille_mask(character).is_none() {
            return false;
        }
        found = true;
    }
    found
}

/// Resolve decoration color using the same snapshot palette as the text.
fn underline_color(cell: &CellSnapshot, colors: &Colors) -> RgbColor {
    let foreground = if cell.style.inverse {
        cell.background
    } else {
        cell.foreground
    };
    match cell.style.underline_color {
        StyleColor::None => foreground,
        StyleColor::Rgb(color) => color,
        StyleColor::Palette(index) => colors
            .palette
            .get(usize::from(index.0))
            .copied()
            .unwrap_or(foreground),
    }
}

/// Locate the cursor in its row, preserving the width of wide graphemes.
fn cursor(snapshot: &TerminalSnapshot, row: u16, focused: bool) -> Option<CursorDrawing> {
    let cursor = snapshot.cursor.as_ref().filter(|cursor| cursor.y == row)?;
    let column = if cursor.at_wide_tail {
        cursor.x.saturating_sub(1)
    } else {
        cursor.x
    };
    let wide = snapshot
        .rows
        .get(usize::from(row))?
        .get(usize::from(column))?
        .width
        == CellWide::Wide;
    Some(CursorDrawing {
        column,
        columns: if wide { WIDE_COLUMNS } else { 1 },
        style: snapshot.cursor_style,
        color: snapshot.colors.cursor.unwrap_or(snapshot.colors.foreground),
        focused,
    })
}
