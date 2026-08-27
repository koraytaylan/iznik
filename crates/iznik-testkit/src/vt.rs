//! The headless VT oracle over `libghostty-vt` 0.2.1, the emulator the
//! macOS application renders with, so "the screen shows X" is a byte-exact
//! assertion through the identical engine rather than an opinion. Snapshots
//! are deterministic and carry no address, capacity or time, so they can be
//! committed as goldens; the goldens live in `tests/fixtures/vt/`.
//!
//! What backs each accessor, at `libghostty-vt` 0.2.1: [`Vt::new`] is
//! `Terminal::new` with `Options { cols, rows, max_scrollback }`; [`Vt::feed`]
//! is `Terminal::vt_write`; [`Vt::resize`] is `Terminal::resize` with nominal
//! cell pixel sizes; [`Vt::cursor`] is `Terminal::cursor_x` and
//! `Terminal::cursor_y`; [`Vt::title`] is `Terminal::title`;
//! [`Vt::working_directory`] is `Terminal::pwd`; [`Vt::scrollback_rows`] is
//! `Terminal::scrollback_rows`; the size is `Terminal::cols` and
//! `Terminal::rows`. A cell is `Terminal::grid_ref` at
//! `Point::Active(PointCoordinate { x: column, y: row })` — the active area
//! is the visible screen, the same cell `Point::Screen` reaches at `y` plus
//! the scrollback rows, and the active lookup is the fast one, which the
//! flood proof needs — then [`Cell::grapheme`] is `GridRef::graphemes`,
//! [`Cell::width`] is `GridRef::cell` then `Cell::wide` (`Narrow`, `Wide`,
//! and `SpacerTail` as [`Width::Continuation`]; a `SpacerHead`, the empty
//! cell before a wide glyph that did not fit the line, is narrow and
//! empty), [`Cell::foreground`], [`Cell::bold`], [`Cell::italic`] and
//! [`Cell::underline`] are `GridRef::style`'s `fg_color`, `bold`, `italic`
//! and `underline`, and [`Cell::background`] is the style's `bg_color`
//! unless `Cell::content_tag` says the cell holds only a background —
//! erased or scrolled in under a background pen, the engine keeps the
//! colour as the cell's content, not its style — in which case it is
//! `Cell::bg_color_palette` or `Cell::bg_color_rgb`.
//!
//! A `Terminal` is not `Send`: a [`Vt`] is created, fed and read on one
//! thread, and a test that needs it under a cap constructs it inside the
//! capped body.

use core::fmt::{self, Display, Formatter, Write as _};

use libghostty_vt::error::Error as EngineError;
use libghostty_vt::screen::{Cell as EngineCell, CellContentTag, CellWide, GridRef, Screen};
use libghostty_vt::style::{StyleColor, Underline as EngineUnderline};
use libghostty_vt::terminal::{Options, Point, PointCoordinate, Terminal};

/// The snapshot format's version, the first line of every snapshot.
pub const SNAPSHOT_VERSION: &str = "vt/1";

/// How many lines of scrollback the oracle keeps: more than any corpus
/// scrolls, fewer than would slow a flood.
const SCROLLBACK_LINES: usize = 10_000;

/// The nominal width of a cell in pixels, which only image protocols and
/// size reports see.
const CELL_WIDTH_PIXELS: u32 = 8;

/// The nominal height of a cell in pixels, which only image protocols and
/// size reports see.
const CELL_HEIGHT_PIXELS: u32 = 16;

/// How many codepoints a grapheme is first asked for; a longer cluster is
/// asked for again with the size the engine names.
const GRAPHEME_CODEPOINTS: usize = 8;

/// Why the oracle could not answer: the engine refused a call.
#[derive(Debug)]
pub struct VtError {
    /// The engine call, as `Terminal::cursor_x`.
    pub call: &'static str,
    /// What the engine said.
    pub source: EngineError,
}

impl Display for VtError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "libghostty-vt {} failed: {}",
            self.call, self.source
        )
    }
}

impl std::error::Error for VtError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Names the engine call an error came from.
///
/// # Errors
///
/// The engine's error, with the call's name.
fn engine<Value>(
    call: &'static str,
    outcome: Result<Value, EngineError>,
) -> Result<Value, VtError> {
    outcome.map_err(|source| VtError { call, source })
}

/// A cell's colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// The terminal's default.
    Default,
    /// One of the 256 palette entries.
    Palette(u8),
    /// A 24-bit colour.
    Rgb {
        /// The red component.
        red: u8,
        /// The green component.
        green: u8,
        /// The blue component.
        blue: u8,
    },
}

impl Display for Color {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Color::Default => write!(formatter, "default"),
            Color::Palette(index) => write!(formatter, "palette({index})"),
            Color::Rgb { red, green, blue } => write!(formatter, "rgb({red},{green},{blue})"),
        }
    }
}

/// A cell's underline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Underline {
    /// None.
    None,
    /// A single line.
    Single,
    /// A double line.
    Double,
    /// A curly line.
    Curly,
    /// A dotted line.
    Dotted,
    /// A dashed line.
    Dashed,
}

impl Display for Underline {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let name = match self {
            Underline::None => "none",
            Underline::Single => "single",
            Underline::Double => "double",
            Underline::Curly => "curly",
            Underline::Dotted => "dotted",
            Underline::Dashed => "dashed",
        };
        write!(formatter, "{name}")
    }
}

/// How many columns a cell spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Width {
    /// One column.
    Narrow,
    /// Two columns: this cell and the continuation after it.
    Wide,
    /// The second column of a wide glyph; it holds no grapheme.
    Continuation,
}

/// One cell of the screen.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    /// The grapheme cluster in the cell; empty for an empty cell and for a
    /// continuation.
    pub grapheme: String,
    /// The foreground colour.
    pub foreground: Color,
    /// The background colour.
    pub background: Color,
    /// Whether the cell is bold.
    pub bold: bool,
    /// Whether the cell is italic.
    pub italic: bool,
    /// The underline.
    pub underline: Underline,
    /// How many columns the cell spans.
    pub width: Width,
}

impl Cell {
    /// The cell's attributes as the snapshot legend lists them, in a fixed
    /// order; empty for a plain narrow cell.
    #[must_use]
    pub fn attributes(&self) -> Vec<String> {
        let mut attributes = Vec::new();
        match self.width {
            Width::Narrow => {}
            Width::Wide => attributes.push("width=wide".to_owned()),
            Width::Continuation => attributes.push("width=continuation".to_owned()),
        }
        if self.bold {
            attributes.push("bold".to_owned());
        }
        if self.italic {
            attributes.push("italic".to_owned());
        }
        if self.underline != Underline::None {
            attributes.push(format!("underline={}", self.underline));
        }
        if self.foreground != Color::Default {
            attributes.push(format!("foreground={}", self.foreground));
        }
        if self.background != Color::Default {
            attributes.push(format!("background={}", self.background));
        }
        attributes
    }

    /// What the cell shows in a row of text: its grapheme, a space when it
    /// is empty, nothing when it is a continuation.
    #[must_use]
    pub fn shown(&self) -> &str {
        match self.width {
            Width::Continuation => "",
            Width::Narrow | Width::Wide if self.grapheme.is_empty() => " ",
            Width::Narrow | Width::Wide => &self.grapheme,
        }
    }
}

/// The oracle's colour for the engine's.
fn color(style: StyleColor) -> Color {
    match style {
        StyleColor::None => Color::Default,
        StyleColor::Palette(index) => Color::Palette(index.0),
        StyleColor::Rgb(rgb) => Color::Rgb {
            red: rgb.r,
            green: rgb.g,
            blue: rgb.b,
        },
    }
}

/// The oracle's underline for the engine's.
///
/// # Errors
///
/// [`VtError`] for a style this version of the oracle does not name, so a
/// newer engine cannot make an underline print as plain.
fn underline(style: EngineUnderline) -> Result<Underline, VtError> {
    match style {
        EngineUnderline::None => Ok(Underline::None),
        EngineUnderline::Single => Ok(Underline::Single),
        EngineUnderline::Double => Ok(Underline::Double),
        EngineUnderline::Curly => Ok(Underline::Curly),
        EngineUnderline::Dotted => Ok(Underline::Dotted),
        EngineUnderline::Dashed => Ok(Underline::Dashed),
        _ => Err(VtError {
            call: "Style::underline",
            source: EngineError::InvalidValue,
        }),
    }
}

/// A cell's background: the style's, unless the cell holds only a
/// background, which the engine keeps as content.
///
/// # Errors
///
/// [`VtError`] when the engine refuses a query.
fn background(raw: EngineCell, style_background: StyleColor) -> Result<Color, VtError> {
    match engine("Cell::content_tag", raw.content_tag())? {
        CellContentTag::BgColorPalette => {
            let index = engine("Cell::bg_color_palette", raw.bg_color_palette())?;
            Ok(Color::Palette(index.0))
        }
        CellContentTag::BgColorRgb => {
            let rgb = engine("Cell::bg_color_rgb", raw.bg_color_rgb())?;
            Ok(Color::Rgb {
                red: rgb.r,
                green: rgb.g,
                blue: rgb.b,
            })
        }
        CellContentTag::Codepoint | CellContentTag::CodepointGrapheme => {
            Ok(color(style_background))
        }
    }
}

/// The grapheme cluster at a grid reference, asked for twice at most.
///
/// # Errors
///
/// [`VtError`] when the engine refuses the query.
fn grapheme(reference: &GridRef<'_>) -> Result<String, VtError> {
    let mut codepoints = ['\0'; GRAPHEME_CODEPOINTS];
    match reference.graphemes(&mut codepoints) {
        Ok(count) => Ok(codepoints.iter().take(count).collect()),
        Err(EngineError::OutOfSpace { required }) => {
            let mut longer = vec!['\0'; required];
            let count = engine("GridRef::graphemes", reference.graphemes(&mut longer))?;
            Ok(longer.iter().take(count).collect())
        }
        Err(source) => Err(VtError {
            call: "GridRef::graphemes",
            source,
        }),
    }
}

/// Appends one line to a snapshot; writing to a `String` cannot fail.
fn put(text: &mut String, line: fmt::Arguments<'_>) {
    let _written = text.write_fmt(line);
    text.push('\n');
}

/// A headless terminal: fed bytes, read as cells, snapshotted as text.
pub struct Vt {
    /// The engine.
    terminal: Terminal<'static, 'static>,
}

impl fmt::Debug for Vt {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("Vt")
    }
}

impl Vt {
    /// A terminal of the given size with an empty screen.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the size, as it does a zero.
    pub fn new(columns: u16, rows: u16) -> Result<Vt, VtError> {
        let terminal = engine(
            "Terminal::new",
            Terminal::new(Options {
                cols: columns,
                rows,
                max_scrollback: SCROLLBACK_LINES,
            }),
        )?;
        Ok(Vt { terminal })
    }

    /// Feeds bytes through the emulator; malformed input is absorbed, never
    /// an error.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Resizes the terminal; the primary screen reflows when wraparound is
    /// on, the alternate screen never does.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the size.
    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<(), VtError> {
        engine(
            "Terminal::resize",
            self.terminal
                .resize(columns, rows, CELL_WIDTH_PIXELS, CELL_HEIGHT_PIXELS),
        )
    }

    /// The size as columns and rows.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the query.
    pub fn size(&self) -> Result<(u16, u16), VtError> {
        Ok((
            engine("Terminal::cols", self.terminal.cols())?,
            engine("Terminal::rows", self.terminal.rows())?,
        ))
    }

    /// The cell at a column and row of the visible screen.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the point is outside the screen or the engine
    /// refuses a query.
    pub fn cell(&self, column: u16, row: u16) -> Result<Cell, VtError> {
        let point = Point::Active(PointCoordinate {
            x: column,
            y: u32::from(row),
        });
        let reference = engine("Terminal::grid_ref", self.terminal.grid_ref(point))?;
        let raw = engine("GridRef::cell", reference.cell())?;
        let style = engine("GridRef::style", reference.style())?;
        let width = match engine("Cell::wide", raw.wide())? {
            CellWide::Wide => Width::Wide,
            CellWide::SpacerTail => Width::Continuation,
            CellWide::Narrow | CellWide::SpacerHead => Width::Narrow,
        };
        let grapheme = match width {
            Width::Continuation => String::new(),
            Width::Narrow | Width::Wide => grapheme(&reference)?,
        };
        Ok(Cell {
            grapheme,
            foreground: color(style.fg_color),
            background: background(raw, style.bg_color)?,
            bold: style.bold,
            italic: style.italic,
            underline: underline(style.underline)?,
            width,
        })
    }

    /// One row as text: each cell as [`Cell::shown`].
    ///
    /// # Errors
    ///
    /// [`VtError`] when the row is outside the screen or the engine refuses
    /// a query.
    pub fn row_text(&self, row: u16) -> Result<String, VtError> {
        let (columns, _rows) = self.size()?;
        let mut text = String::new();
        for column in 0..columns {
            text.push_str(self.cell(column, row)?.shown());
        }
        Ok(text)
    }

    /// The screen as text, rows joined by newlines.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses a query.
    pub fn screen_text(&self) -> Result<String, VtError> {
        let (_columns, rows) = self.size()?;
        let lines: Vec<String> = (0..rows)
            .map(|row| self.row_text(row))
            .collect::<Result<_, _>>()?;
        Ok(lines.join("\n"))
    }

    /// The cursor as column and row, zero-based.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the query.
    pub fn cursor(&self) -> Result<(u16, u16), VtError> {
        Ok((
            engine("Terminal::cursor_x", self.terminal.cursor_x())?,
            engine("Terminal::cursor_y", self.terminal.cursor_y())?,
        ))
    }

    /// The title set by OSC 0 or 2; empty when none was.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the query.
    pub fn title(&self) -> Result<String, VtError> {
        engine("Terminal::title", self.terminal.title()).map(str::to_owned)
    }

    /// The OSC 7 payload as sent — a URI such as `file://host/tmp/work`,
    /// unparsed; whoever wants the path parses it — or empty when none was.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the query.
    pub fn working_directory(&self) -> Result<String, VtError> {
        engine("Terminal::pwd", self.terminal.pwd()).map(str::to_owned)
    }

    /// Whether the alternate screen — the one a full-screen program switches to
    /// and that never reflows — is the active one.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the query.
    pub fn in_alternate_screen(&self) -> Result<bool, VtError> {
        let screen = engine("Terminal::active_screen", self.terminal.active_screen())?;
        Ok(screen == Screen::Alternate)
    }

    /// How many rows have scrolled off the top of the screen.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses the query.
    pub fn scrollback_rows(&self) -> Result<usize, VtError> {
        engine("Terminal::scrollback_rows", self.terminal.scrollback_rows())
    }

    /// The `vt/1` snapshot: the version, the size, the cursor, the title
    /// and working directory quoted, the scrollback count, one framed line
    /// per row, and a legend of every cell that is not plain, in row-major
    /// order. No address, capacity, time or hash order appears in it.
    ///
    /// # Errors
    ///
    /// [`VtError`] when the engine refuses a query.
    pub fn snapshot(&self) -> Result<String, VtError> {
        let (columns, rows) = self.size()?;
        let (cursor_column, cursor_row) = self.cursor()?;
        let mut text = String::new();
        put(&mut text, format_args!("{SNAPSHOT_VERSION}"));
        put(&mut text, format_args!("size {columns}x{rows}"));
        put(
            &mut text,
            format_args!("cursor {cursor_column},{cursor_row}"),
        );
        put(&mut text, format_args!("title {:?}", self.title()?));
        put(
            &mut text,
            format_args!("working_directory {:?}", self.working_directory()?),
        );
        put(
            &mut text,
            format_args!("scrollback {}", self.scrollback_rows()?),
        );
        let mut legend = String::new();
        for row in 0..rows {
            let mut line = String::new();
            for column in 0..columns {
                let cell = self.cell(column, row)?;
                line.push_str(cell.shown());
                let attributes = cell.attributes();
                if !attributes.is_empty() {
                    put(
                        &mut legend,
                        format_args!("{column},{row} {}", attributes.join(" ")),
                    );
                }
            }
            put(&mut text, format_args!("{row}|{line}|"));
        }
        text.push_str("attributes\n");
        text.push_str(&legend);
        Ok(text)
    }
}
