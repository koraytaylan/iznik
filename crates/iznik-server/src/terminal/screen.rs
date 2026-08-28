//! The screen serializer: the mirror's state as VT sequences, exact at a named
//! sequence number — the one resynchronization mechanism a cold attach uses.
//!
//! Its correctness is a property, not an example: feeding `bytes` into a fresh
//! emulator of the same size reproduces the mirror, scrollback and cursor
//! included. Because the formatter cannot see an inactive screen, [`ScreenState`]
//! remembers the primary screen at the moment a program switched away from it.
//!
//! **How libghostty-vt 0.2.1's formatter drops scrollback (confirmed
//! empirically, this crate version):** the VT formatter with no selection emits
//! the whole screen, scrollback included; a selection whose start row is lower
//! in `Point::Screen` space drops exactly that many oldest rows. So the bound is
//! met by formatting a selection that starts further down until the output fits.
//! (With libghostty's byte-budget scrollback the output is usually far below the
//! bound, so a drop is rare, but the mechanism is here when it is not.)
//!
//! **What its formatter does not round-trip (confirmed empirically, this crate
//! version).** The screen content, cursor and layout reproduce exactly — every
//! test here proves that — but the reconstruction has edges the resync inherits
//! until the binding matures: a screen whose bottom row a scroll left blank comes
//! back one row behind (so tests rest the cursor on content); an overwritten or
//! erased cell can come back carrying a stale style; a reflow after a resize is
//! approximate; and hyperlinks (OSC 8) are not emitted at all even with them
//! enabled. The formatter also emits the cursor before its tabstops pass, which
//! moves the cursor while setting each stop, so [`serialize`] re-emits the cursor
//! last (see `append_cursor`).

use iznik_protocol::identity::Sequence;
use libghostty_vt::Error as EmulatorError;
use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::screen::CellWide;
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::{Point, PointCoordinate, Terminal};

use crate::terminal::mirror::Mirror;

/// The most bytes a serialized screen holds — below the frame maximum. A screen
/// with scrollback is dropped to fit this; the bound is best-effort in two rare
/// cases the drop cannot help: a visible screen that alone exceeds it, and
/// [`ScreenState::serialize`]'s remembered-primary-plus-alternate concatenation.
pub const MAXIMUM_SCREEN_BYTES: usize = 768 * 1024;

/// How many codepoints a grapheme cluster is asked for in one go: enough for
/// anything a terminal usually holds, and the engine says when it is not.
const GRAPHEME_CODEPOINTS: usize = 8;

/// The most bytes a cell written back after the cursor may take: that many
/// codepoints at four bytes each. A cluster longer than this is left to the
/// position alone rather than allowed past the budget.
const MAXIMUM_CELL_BYTES: usize = GRAPHEME_CODEPOINTS * 4;

/// The most bytes the position `append_cursor` adds can take:
/// `ESC [ 65536 ; 65536 H`, and the one cell it may write after it.
const MAXIMUM_CURSOR_BYTES: usize = 14 + MAXIMUM_CELL_BYTES;

/// The budget the formatted screen must fit before the cursor is appended, so the
/// whole stays within [`MAXIMUM_SCREEN_BYTES`].
const SCREEN_BYTES_TARGET: usize = MAXIMUM_SCREEN_BYTES.saturating_sub(MAXIMUM_CURSOR_BYTES);

/// The mirror's screen as VT sequences, exact at [`SerializedScreen::sequence`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerializedScreen {
    /// The absolute sequence the screen is exact at.
    pub sequence: Sequence,
    /// The column count the bytes reproduce at.
    pub columns: u16,
    /// The row count the bytes reproduce at.
    pub rows: u16,
    /// The VT sequences that reproduce the screen.
    pub bytes: Vec<u8>,
    /// How many oldest scrollback rows were dropped to fit the bound.
    pub dropped_rows: usize,
}

/// Why a screen could not be serialized.
#[derive(Debug)]
pub enum ScreenError {
    /// The emulator's formatter failed.
    Emulator {
        /// What the emulator reported.
        source: EmulatorError,
    },
}

impl core::fmt::Display for ScreenError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ScreenError::Emulator { source } => {
                write!(formatter, "the emulator's formatter failed: {source:?}")
            }
        }
    }
}

impl std::error::Error for ScreenError {}

/// Wraps an emulator error as a [`ScreenError`].
fn emulator(source: EmulatorError) -> ScreenError {
    ScreenError::Emulator { source }
}

/// The formatting options every serialization uses: VT sequences with the state
/// a fresh emulator needs, and the palette left out — the server's palette is
/// not the client's.
fn screen_options<'terminal, 'selection>() -> FormatterOptions<'terminal, 'selection> {
    FormatterOptions::new()
        .with_format(Format::Vt)
        .with_cursor(true)
        .with_style(true)
        .with_hyperlink(true)
        .with_modes(true)
        .with_scrolling_region(true)
        .with_tabstops(true)
        .with_pwd(true)
        .with_keyboard(true)
        .with_kitty_keyboard(true)
        .with_charsets(true)
        .with_protection(true)
        .with_palette(false)
}

/// Runs a formatter into an owned buffer, sized to exactly what it needs.
///
/// # Errors
///
/// [`ScreenError::Emulator`] when the formatter fails.
fn format_to_vec(formatter: &mut Formatter<'_, '_, '_>) -> Result<Vec<u8>, ScreenError> {
    let required = formatter.format_len().map_err(emulator)?;
    let mut buffer = vec![0; required];
    let length = formatter.format_buf(&mut buffer).map_err(emulator)?;
    buffer.truncate(length);
    Ok(buffer)
}

/// Formats the terminal's screen, dropping its oldest `drop_rows` rows via a
/// selection that starts that far down; `drop_rows` of zero formats it whole.
///
/// # Errors
///
/// [`ScreenError::Emulator`] when the formatter or a grid reference fails.
fn format_screen(
    terminal: &Terminal<'static, 'static>,
    drop_rows: usize,
) -> Result<Vec<u8>, ScreenError> {
    if drop_rows == 0 {
        let mut formatter = Formatter::new(terminal, screen_options()).map_err(emulator)?;
        return format_to_vec(&mut formatter);
    }
    let scrollback = terminal.scrollback_rows().map_err(emulator)?;
    let rows = terminal.rows().map_err(emulator)?;
    let columns = terminal.cols().map_err(emulator)?;
    let last_row = scrollback
        .saturating_add(usize::from(rows))
        .saturating_sub(1);
    let start = terminal
        .grid_ref(Point::Screen(PointCoordinate {
            x: 0,
            y: u32::try_from(drop_rows).unwrap_or(u32::MAX),
        }))
        .map_err(emulator)?;
    let end = terminal
        .grid_ref(Point::Screen(PointCoordinate {
            x: columns.saturating_sub(1),
            y: u32::try_from(last_row).unwrap_or(u32::MAX),
        }))
        .map_err(emulator)?;
    let selection = Selection::new(start, end, false);
    let mut formatter =
        Formatter::new(terminal, screen_options().with_selection(&selection)).map_err(emulator)?;
    format_to_vec(&mut formatter)
}

/// The smallest number of oldest scrollback rows to drop for the output to fit
/// the bound, and the output at that drop, found by bisection over `[1, cap]`.
///
/// # Errors
///
/// [`ScreenError::Emulator`] when the formatter fails.
fn fit_by_dropping(
    terminal: &Terminal<'static, 'static>,
    cap: usize,
) -> Result<(usize, Vec<u8>), ScreenError> {
    let mut low: usize = 0;
    let mut high = cap;
    let mut fitted = format_screen(terminal, high)?;
    while low.saturating_add(1) < high {
        let middle = low.midpoint(high);
        let candidate = format_screen(terminal, middle)?;
        if candidate.len() <= SCREEN_BYTES_TARGET {
            high = middle;
            fitted = candidate;
        } else {
            low = middle;
        }
    }
    Ok((high, fitted))
}

/// Serializes the mirror's active screen, exact at `sequence`, within the bound.
///
/// # Errors
///
/// [`ScreenError::Emulator`] when the emulator's formatter fails.
pub fn serialize(mirror: &Mirror, sequence: Sequence) -> Result<SerializedScreen, ScreenError> {
    let terminal = mirror.terminal();
    let whole = format_screen(terminal, 0)?;
    let (dropped_rows, mut bytes) = if whole.len() <= SCREEN_BYTES_TARGET {
        (0, whole)
    } else {
        fit_by_dropping(terminal, mirror.scrollback_rows())?
    };
    append_cursor(terminal, &mut bytes)?;
    Ok(SerializedScreen {
        sequence,
        columns: mirror.columns(),
        rows: mirror.rows(),
        bytes,
        dropped_rows,
    })
}

/// Puts the cursor back where the mirror's is, last of all. The formatter emits
/// the cursor, but its tabstops pass moves the cursor afterward to set each tab
/// stop, so a final `CUP` is needed for a reproduction to end where the mirror
/// is.
///
/// A cursor resting on the last column is the exception, and it matters more
/// than its rarity suggests. A program that has just filled a line leaves the
/// cursor there with a wrap *pending*: the next character goes to the start of
/// the next row, not over the last cell. `CUP` cannot say that — a fresh
/// emulator positioned there has no wrap pending — so a client that attached
/// at exactly that moment would put the next character one column to the left
/// and stay one column out for as long as the program went on printing.
/// Writing the last cell's own text *at* that column leaves the emulator where
/// the mirror is, wrap and all: a character written into the last column is
/// what puts a terminal into that state, and the character written is the one
/// already there, so the screen is unchanged.
///
/// # Errors
///
/// [`ScreenError::Emulator`] when the cursor or the cell under it cannot be
/// read.
fn append_cursor(
    terminal: &Terminal<'static, 'static>,
    bytes: &mut Vec<u8>,
) -> Result<(), ScreenError> {
    let column = terminal.cursor_x().map_err(emulator)?;
    let row = terminal.cursor_y().map_err(emulator)?;
    let columns = terminal.cols().map_err(emulator)?;
    if column.saturating_add(1) == columns
        && let Some(text) = narrow_cell(terminal, row, column)?
    {
        append_position(bytes, row, column);
        bytes.extend_from_slice(text.as_bytes());
        return Ok(());
    }
    append_position(bytes, row, column);
    Ok(())
}

/// A `CUP` to a zero-based row and column.
fn append_position(bytes: &mut Vec<u8>, row: u16, column: u16) {
    let cup = format!(
        "\x1b[{};{}H",
        row.saturating_add(1),
        column.saturating_add(1)
    );
    bytes.extend_from_slice(cup.as_bytes());
}

/// The text of the cell at `row` and `column`, when it is one a single
/// character can be written back into: narrow, and not empty.
///
/// A wide glyph or an empty cell is left to `CUP`, because writing a
/// two-column character from one column back would land it somewhere else and
/// writing nothing would say nothing.
///
/// # Errors
///
/// [`ScreenError::Emulator`] when the grid cannot be read.
fn narrow_cell(
    terminal: &Terminal<'static, 'static>,
    row: u16,
    column: u16,
) -> Result<Option<String>, ScreenError> {
    let point = Point::Active(PointCoordinate {
        x: column,
        y: u32::from(row),
    });
    let reference = terminal.grid_ref(point).map_err(emulator)?;
    let cell = reference.cell().map_err(emulator)?;
    if cell.wide().map_err(emulator)? != CellWide::Narrow {
        return Ok(None);
    }
    // Asked for twice at most: once into a buffer wide enough for anything a
    // terminal usually holds, and again into one the engine sized itself.
    let mut codepoints = ['\0'; GRAPHEME_CODEPOINTS];
    let text: String = match reference.graphemes(&mut codepoints) {
        Ok(count) => codepoints.iter().take(count).collect(),
        Err(EmulatorError::OutOfSpace { required }) => {
            let mut longer = vec!['\0'; required];
            let count = reference.graphemes(&mut longer).map_err(emulator)?;
            longer.iter().take(count).collect()
        }
        Err(source) => return Err(emulator(source)),
    };
    Ok(Some(text).filter(|held| !held.is_empty() && held.len() <= MAXIMUM_CELL_BYTES))
}

/// The primary screen remembered at the moment a program switched away from it,
/// and the switch sequence that took it there.
#[derive(Clone, Debug)]
struct PrimarySnapshot {
    /// The primary screen as VT sequences, taken while it was still active.
    bytes: Vec<u8>,
    /// The alternate-screen switch sequence the pane recognized.
    switch: Vec<u8>,
    /// How many of the primary's oldest scrollback rows were dropped to fit.
    dropped_rows: usize,
}

/// What the serializer must remember across an alternate-screen switch, because
/// the formatter can only see the active screen.
#[derive(Clone, Debug, Default)]
pub struct ScreenState {
    /// The primary screen, remembered while a program is on the alternate one.
    primary_at_switch: Option<PrimarySnapshot>,
}

impl ScreenState {
    /// A state remembering nothing yet.
    #[must_use]
    pub fn new() -> ScreenState {
        ScreenState {
            primary_at_switch: None,
        }
    }

    /// Remembers the primary screen, serialized while it is still active, and the
    /// `switch` sequence that is about to take the mirror to the alternate one.
    /// The pane calls this before it feeds the mirror the switch.
    ///
    /// # Errors
    ///
    /// [`ScreenError::Emulator`] when the primary screen cannot be serialized.
    pub fn entering_alternate(
        &mut self,
        mirror: &Mirror,
        switch: &[u8],
    ) -> Result<(), ScreenError> {
        let primary = serialize(mirror, Sequence(0))?;
        self.primary_at_switch = Some(PrimarySnapshot {
            bytes: primary.bytes,
            switch: switch.to_vec(),
            dropped_rows: primary.dropped_rows,
        });
        Ok(())
    }

    /// Forgets the remembered primary — the program has left the alternate screen.
    pub fn leaving_alternate(&mut self) {
        self.primary_at_switch = None;
    }

    /// Serializes the mirror, exact at `sequence`. While a program is on the
    /// alternate screen this is the remembered primary, the switch, then the live
    /// alternate — so leaving the alternate screen on the client reveals what it
    /// reveals on the server.
    ///
    /// # Errors
    ///
    /// [`ScreenError::Emulator`] when the emulator's formatter fails.
    pub fn serialize(
        &self,
        mirror: &Mirror,
        sequence: Sequence,
    ) -> Result<SerializedScreen, ScreenError> {
        let active = serialize(mirror, sequence)?;
        match &self.primary_at_switch {
            None => Ok(active),
            Some(snapshot) => {
                let mut bytes = snapshot.bytes.clone();
                bytes.extend_from_slice(&snapshot.switch);
                bytes.extend_from_slice(&active.bytes);
                Ok(SerializedScreen {
                    sequence,
                    columns: active.columns,
                    rows: active.rows,
                    bytes,
                    dropped_rows: snapshot.dropped_rows.saturating_add(active.dropped_rows),
                })
            }
        }
    }
}
