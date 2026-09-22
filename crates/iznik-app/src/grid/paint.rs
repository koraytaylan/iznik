//! The custom terminal row element: GPUI owns shaping and every glyph resource.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    App, Bounds, ContentMask, Context, Element, ElementId, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Render, ShapedLine,
    StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill, font, point, px,
    rgb, size,
};
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::style::{RgbColor, Underline};

use super::{
    CellRun, GridMetrics, RowDrawing, braille_mask, distance, is_braille_run, is_symbol_text,
    offset, scale,
};

/// Half-opacity faint text, matching the common terminal faint rendition.
const FAINT_OPACITY: f32 = 0.5;
/// Selection tint retains the underlying text and cell background.
const SELECTION_COLOR: u32 = 0x004c_7fbf;
/// Selection opacity leaves inverse and colored cells legible.
const SELECTION_OPACITY: f32 = 0.4;
/// A block cursor is translucent so the grapheme under it remains visible.
const CURSOR_OPACITY: f32 = 0.5;
/// Logical pixel width for a bar, underline or hollow cursor stroke.
const STROKE_WIDTH: f32 = 1.0;
/// Double underlines leave one stroke of space between their rules.
const DOUBLE_OFFSET: f32 = 2.0;
/// Dotted underlines repeat at half-cell intervals.
const DOT_INTERVAL: f32 = 0.5;
/// Dashed underlines fill three quarters of each cell.
const DASH_FRACTION: f32 = 0.75;
/// Half a span. Symbol glyphs are centered when their advance is not the cell.
const HALF: f32 = 0.5;
/// Byte position of red in an RGB integer.
const RED_SHIFT: u32 = 16;
/// Byte position of green in an RGB integer.
const GREEN_SHIFT: u32 = 8;
/// Braille patterns place dots in two columns.
const BRAILLE_DOT_COLUMNS: u32 = 2;
/// Braille patterns place dots in four rows.
const BRAILLE_DOT_ROWS: u32 = 4;
/// Gaps between the four braille rows. Growing the row pitch costs one pixel per gap.
const BRAILLE_ROW_GAPS: u32 = 3;
/// The three upper rows of a pattern; the last two dots sit on the bottom row.
const BRAILLE_UPPER_ROWS: u32 = 3;
/// Dot bits that belong to the three upper rows, three per column.
const BRAILLE_UPPER_COUNT: u32 = 6;
/// Vertical index of the bottom braille row.
const BRAILLE_BOTTOM_ROW: u16 = 3;
/// Horizontal quarters of a cell: two dot columns and the gaps around them.
const BRAILLE_HORIZONTAL_PARTS: u32 = 4;
/// Vertical eighths of a cell: four dot rows and the gaps around them.
const BRAILLE_VERTICAL_PARTS: u32 = 8;
/// Two margins, one on each side. Growing a margin by one pixel costs both.
const BRAILLE_MARGIN_COUNT: u32 = 2;

/// Convert the emulator's resolved RGB without losing a channel or alpha.
pub(crate) fn color(value: RgbColor) -> Hsla {
    rgb((u32::from(value.r) << RED_SHIFT)
        | (u32::from(value.g) << GREEN_SHIFT)
        | u32::from(value.b))
    .into()
}

/// Framework-cached row owner; notifications invalidate only this subtree.
#[derive(Debug)]
pub(super) struct RowView {
    /// Current draw list compared by the grid before notifying.
    pub(super) drawing: Arc<RowDrawing>,
    /// Font and geometry shared with the grid's hit testing.
    metrics: GridMetrics,
    /// Count actual custom-element paint calls, excluding reused subtrees.
    pub(super) paint_count: Rc<Cell<u64>>,
    /// A shaping or painting failure remains available to application diagnostics.
    pub(super) error: Rc<RefCell<Option<String>>>,
}

impl RowView {
    /// Construct one row without allocating any glyph atlas or emulator.
    pub(super) fn new(drawing: RowDrawing, metrics: GridMetrics) -> Self {
        Self {
            drawing: Arc::new(drawing),
            metrics,
            paint_count: Rc::new(Cell::new(0)),
            error: Rc::new(RefCell::new(None)),
        }
    }

    /// Replace this row's font and cell geometry for its next paint.
    pub(super) fn set_metrics(&mut self, metrics: &GridMetrics) {
        self.metrics = metrics.clone();
    }
}

impl Render for RowView {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        GridElement {
            drawing: Arc::clone(&self.drawing),
            metrics: self.metrics.clone(),
            paint_count: Rc::clone(&self.paint_count),
            error: Rc::clone(&self.error),
        }
    }
}

/// Custom element for one cached terminal row.
struct GridElement {
    /// Immutable owned cell runs for this frame.
    drawing: Arc<RowDrawing>,
    /// One source for font size and cell geometry.
    metrics: GridMetrics,
    /// Visible paint counter shared with its entity.
    paint_count: Rc<Cell<u64>>,
    /// Last paint failure shared with diagnostics.
    error: Rc<RefCell<Option<String>>>,
}

/// One shaped line anchored at its terminal column, rather than the previous glyph.
struct GlyphRun {
    /// Starting terminal column.
    column: u16,
    /// Columns this line must occupy. A symbol is centered and clipped to them.
    columns: u16,
    /// Center the shaped advance inside [`Self::columns`] and clip to that span.
    center: bool,
    /// GPUI's shaped glyphs and text decorations.
    line: ShapedLine,
}

impl IntoElement for GridElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = Vec<GlyphRun>;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        context: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(
                (scale(self.metrics.cell_width, f32::from(self.drawing.columns))).into(),
                self.metrics.line_height.into(),
            ),
            flex_shrink: 0.0,
            ..Style::default()
        };
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        _context: &mut App,
    ) -> Vec<GlyphRun> {
        shape(&self.drawing, &self.metrics, window)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        shaped: &mut Vec<GlyphRun>,
        window: &mut Window,
        context: &mut App,
    ) {
        self.paint_count
            .set(self.paint_count.get().saturating_add(1));
        *self.error.borrow_mut() = None;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            background(&self.drawing, &self.metrics, bounds, window);
            for glyph in shaped {
                if let Err(error) =
                    paint_shaped(glyph, &self.metrics, bounds.origin, window, context)
                {
                    *self.error.borrow_mut() = Some(error);
                }
            }
            braille(&self.drawing, &self.metrics, bounds.origin, window);
            decorations(&self.drawing, &self.metrics, bounds.origin, window);
            cursor(&self.drawing, &self.metrics, bounds.origin, window);
        });
    }
}

/// Paint one shaped run. Symbol glyphs are centered in their cells and clipped
/// there, so a row of small squares stays one dot per column.
///
/// # Errors
/// Returns the text-system message when a glyph cannot be painted.
fn paint_shaped(
    glyph: &GlyphRun,
    metrics: &GridMetrics,
    origin: Point<Pixels>,
    window: &mut Window,
    context: &mut App,
) -> Result<(), String> {
    let bounds = cell_bounds(metrics, origin, glyph.column, glyph.columns);
    let mut glyph_origin = bounds.origin;
    if glyph.center {
        glyph_origin.x = offset(
            glyph_origin.x,
            scale(distance(bounds.size.width, glyph.line.width()), HALF),
        );
    }
    let painted = if glyph.center {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            glyph.line.paint(
                glyph_origin,
                metrics.line_height,
                TextAlign::Left,
                None,
                window,
                context,
            )
        })
    } else {
        glyph.line.paint(
            glyph_origin,
            metrics.line_height,
            TextAlign::Left,
            None,
            window,
            context,
        )
    };
    painted.map_err(|error| error.to_string())
}

/// Shape complete narrow runs so font ligatures remain intact. Wide graphemes,
/// braille patterns and geometric symbols are separate runs. Wide runs keep the
/// following column independent of fallback font advances. Braille is painted
/// as a dot grid. Symbols are centered in their cell, because a spinner's
/// squares and dots have a smaller advance than the cell and would otherwise
/// pile up. GPUI's force-width mode is deliberately absent: it assigns one
/// column per glyph and would collapse multi-cell ligatures.
fn shape(drawing: &RowDrawing, metrics: &GridMetrics, window: &Window) -> Vec<GlyphRun> {
    drawing
        .runs
        .iter()
        .filter(|run| !run.style.invisible && !is_braille_run(&run.text))
        .map(|run| {
            let mut selected_font = font(metrics.font.clone());
            if run.style.bold {
                selected_font = selected_font.bold();
            }
            if run.style.italic {
                selected_font = selected_font.italic();
            }
            let foreground = run_foreground(run);
            let underline = (run.style.underline == Underline::Curly).then_some(UnderlineStyle {
                thickness: px(STROKE_WIDTH),
                color: Some(color(run.underline)),
                wavy: true,
            });
            let strike = run.style.strikethrough.then_some(StrikethroughStyle {
                thickness: px(STROKE_WIDTH),
                color: Some(foreground),
            });
            let text_run = TextRun {
                len: run.text.len(),
                font: selected_font,
                color: foreground,
                background_color: None,
                underline,
                strikethrough: strike,
            };
            GlyphRun {
                column: run.column,
                columns: run.columns,
                center: is_symbol_text(&run.text),
                line: window.text_system().shape_line(
                    run.text.clone().into(),
                    metrics.font_size,
                    &[text_run],
                    None,
                ),
            }
        })
        .collect()
}

/// Effective text color, including inverse video and faint rendition.
fn run_foreground(run: &CellRun) -> Hsla {
    let mut foreground = color(if run.style.inverse {
        run.background
    } else {
        run.foreground
    });
    if run.style.faint {
        foreground.a *= FAINT_OPACITY;
    }
    foreground
}

/// One row's braille geometry in whole device pixels, shared by every column.
struct BrailleGrid {
    /// Display pixels per logical pixel. Positions divide back by this.
    scale: f32,
    /// Logical row origin, the same one text cells use.
    origin: Point<Pixels>,
    /// Logical width of one terminal column.
    cell_width: Pixels,
    /// Logical height of the row.
    line_height: Pixels,
    /// Smallest device-pixel width a column on this row can snap to.
    column_device: u32,
    /// Smallest device-pixel height this row can snap to.
    row_device: u32,
    /// Width and height of every dot, in device pixels.
    dot: u32,
    /// Inset of the left dot column, in device pixels.
    horizontal_margin: u32,
    /// Inset of the top dot row, in device pixels.
    vertical_margin: u32,
    /// Gap between the two dot columns, in device pixels.
    horizontal_spacing: u32,
    /// Gap between consecutive dot rows, in device pixels.
    vertical_spacing: u32,
}

/// Paint braille patterns inside their terminal cells.
fn braille(
    drawing: &RowDrawing,
    metrics: &GridMetrics,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let Some(grid) = braille_grid(window, metrics, origin) else {
        return;
    };
    for run in drawing
        .runs
        .iter()
        .filter(|run| !run.style.invisible && is_braille_run(&run.text))
    {
        let tint = run_foreground(run);
        for (index, character) in run.text.chars().enumerate() {
            let Some(mask) = braille_mask(character) else {
                continue;
            };
            let Some(offset_columns) = u16::try_from(index).ok() else {
                continue;
            };
            let Some(column) = run.column.checked_add(offset_columns) else {
                continue;
            };
            paint_braille_cell(window, &grid, column, mask, tint);
        }
    }
}

/// Draw the set dots of one pattern. Every dot is the same whole device pixels,
/// and each pattern is clipped to the terminal cell text uses, so a chart cannot
/// walk into the following label.
fn paint_braille_cell(window: &mut Window, grid: &BrailleGrid, column: u16, mask: u8, tint: Hsla) {
    let bounds = Bounds::new(
        point(
            offset(grid.origin.x, scale(grid.cell_width, f32::from(column))),
            grid.origin.y,
        ),
        size(grid.cell_width, grid.line_height),
    );
    let dot = from_device(grid.dot, grid.scale);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for index in 0..u8::BITS {
            let Some(dot_mask) = 1_u8.checked_shl(index) else {
                continue;
            };
            if (mask & dot_mask) == 0 {
                continue;
            }
            let (dot_column, dot_row) = braille_place(index);
            let dot_origin = braille_point(
                grid,
                column,
                braille_offset(
                    u32::from(dot_column),
                    grid.horizontal_margin,
                    grid.dot,
                    grid.horizontal_spacing,
                ),
                braille_offset(
                    u32::from(dot_row),
                    grid.vertical_margin,
                    grid.dot,
                    grid.vertical_spacing,
                ),
            );
            window.paint_quad(fill(Bounds::new(dot_origin, size(dot, dot)), tint));
        }
    });
}

/// Device-pixel offset of one dot inside its cell.
fn braille_offset(index: u32, margin: u32, dot: u32, spacing: u32) -> u32 {
    margin.saturating_add(index.saturating_mul(dot.saturating_add(spacing)))
}

/// Column and row of one braille bit. Bits 0 through 5 fill the upper rows
/// from top to bottom, left column then right; bits 6 and 7 are the bottom row.
fn braille_place(index: u32) -> (u16, u16) {
    if index < BRAILLE_UPPER_COUNT {
        let column = index.checked_div(BRAILLE_UPPER_ROWS).unwrap_or(0);
        let row = index.checked_rem(BRAILLE_UPPER_ROWS).unwrap_or(0);
        (
            u16::try_from(column).unwrap_or(0),
            u16::try_from(row).unwrap_or(0),
        )
    } else {
        (
            u16::try_from(index.saturating_sub(BRAILLE_UPPER_COUNT)).unwrap_or(0),
            BRAILLE_BOTTOM_ROW,
        )
    }
}

/// Snap the row onto the display pixel grid and choose one dot size for it.
fn braille_grid(
    window: &Window,
    metrics: &GridMetrics,
    origin: Point<Pixels>,
) -> Option<BrailleGrid> {
    let scale = window.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    // Floor, rather than round. Rounded columns drift apart from the text grid,
    // and the wider ones would paint into the next cell.
    let column_device = device_floor(metrics.cell_width, scale);
    let row_device = device_floor(metrics.line_height, scale);
    if column_device == 0 || row_device == 0 {
        return None;
    }
    let mut grid = BrailleGrid {
        scale,
        origin,
        cell_width: metrics.cell_width,
        line_height: metrics.line_height,
        column_device,
        row_device,
        dot: column_device
            .checked_div(BRAILLE_HORIZONTAL_PARTS)
            .unwrap_or(0)
            .min(row_device.checked_div(BRAILLE_VERTICAL_PARTS).unwrap_or(0)),
        horizontal_spacing: column_device
            .checked_div(BRAILLE_HORIZONTAL_PARTS)
            .unwrap_or(0),
        vertical_spacing: row_device.checked_div(BRAILLE_VERTICAL_PARTS).unwrap_or(0),
        horizontal_margin: 0,
        vertical_margin: 0,
    };
    grid.horizontal_margin = grid
        .horizontal_spacing
        .checked_div(BRAILLE_MARGIN_COUNT)
        .unwrap_or(0);
    grid.vertical_margin = grid
        .vertical_spacing
        .checked_div(BRAILLE_MARGIN_COUNT)
        .unwrap_or(0);
    fit_braille(&mut grid);
    (grid.dot > 0).then_some(grid)
}

/// Logical position of a device-pixel offset inside one terminal cell.
///
/// The cell edge is snapped on its own. Multiplying a rounded column width
/// would walk the dots into later columns.
fn braille_point(grid: &BrailleGrid, column: u16, horizontal: u32, vertical: u32) -> Point<Pixels> {
    let left = device_count(
        offset(grid.origin.x, scale(grid.cell_width, f32::from(column))),
        grid.scale,
    );
    let top = device_count(grid.origin.y, grid.scale);
    point(
        from_device(left.saturating_add(horizontal), grid.scale),
        from_device(top.saturating_add(vertical), grid.scale),
    )
}

/// Largest whole device-pixel span that fits inside a logical length.
///
/// Snapped cell edges differ by either this or one pixel more. Dots laid out
/// in this span fit the narrower cells and stay the same size in the wider ones.
fn device_floor(extent: Pixels, scale: f32) -> u32 {
    let value = (f32::from(extent) * scale).floor().max(0.0);
    u32::from(px(value))
}

/// Round a logical length to whole device pixels the way the renderer snaps quad edges.
///
/// The renderer rounds each edge independently, with half-pixel ties toward zero.
/// A fractional dot therefore covers one more pixel in some columns than others.
/// Counting in device pixels first keeps every dot the same number of pixels.
fn device_count(extent: Pixels, scale: f32) -> u32 {
    let value = f32::from(extent) * scale;
    let value = (value.abs() - HALF).ceil().copysign(value);
    u32::from(px(value.max(0.0)))
}

/// Convert a device-pixel count back to a logical offset on that same grid.
fn from_device(count: u32, factor: f32) -> Pixels {
    scale(Pixels::from(count), 1.0 / factor)
}

/// Give leftover device pixels to margins, spacing and dot size, one pixel at a time.
fn fit_braille(grid: &mut BrailleGrid) {
    let mut horizontal = horizontal_slack(grid);
    let mut vertical = vertical_slack(grid);
    if grid.dot == 0 {
        give_dot(&mut horizontal, &mut vertical, &mut grid.dot);
    }
    if grid.horizontal_margin == 0 {
        give(
            &mut horizontal,
            BRAILLE_MARGIN_COUNT,
            &mut grid.horizontal_margin,
        );
    }
    if grid.vertical_margin == 0 {
        give(
            &mut vertical,
            BRAILLE_MARGIN_COUNT,
            &mut grid.vertical_margin,
        );
    }
    give(&mut horizontal, 1, &mut grid.horizontal_spacing);
    give(&mut vertical, BRAILLE_ROW_GAPS, &mut grid.vertical_spacing);
    give(
        &mut horizontal,
        BRAILLE_MARGIN_COUNT,
        &mut grid.horizontal_margin,
    );
    give(
        &mut vertical,
        BRAILLE_MARGIN_COUNT,
        &mut grid.vertical_margin,
    );
    give_dot(&mut horizontal, &mut vertical, &mut grid.dot);
}

/// Spend leftover space on a one-pixel increase when both axes can afford a larger dot.
fn give_dot(horizontal: &mut u32, vertical: &mut u32, dot: &mut u32) {
    if *horizontal >= BRAILLE_MARGIN_COUNT && *vertical >= BRAILLE_DOT_ROWS {
        *dot = dot.saturating_add(1);
        *horizontal = horizontal.saturating_sub(BRAILLE_MARGIN_COUNT);
        *vertical = vertical.saturating_sub(BRAILLE_DOT_ROWS);
    }
}

/// Spend leftover space on a one-pixel increase of one placement.
fn give(remaining: &mut u32, needed: u32, slot: &mut u32) {
    if *remaining >= needed {
        *slot = slot.saturating_add(1);
        *remaining = remaining.saturating_sub(needed);
    }
}

/// Unused device pixels after margins, the column gap and both dots.
fn horizontal_slack(grid: &BrailleGrid) -> u32 {
    grid.column_device
        .saturating_sub(BRAILLE_MARGIN_COUNT.saturating_mul(grid.horizontal_margin))
        .saturating_sub(grid.horizontal_spacing)
        .saturating_sub(BRAILLE_DOT_COLUMNS.saturating_mul(grid.dot))
}

/// Unused device pixels after margins, the row gaps and all four dots.
fn vertical_slack(grid: &BrailleGrid) -> u32 {
    grid.row_device
        .saturating_sub(BRAILLE_MARGIN_COUNT.saturating_mul(grid.vertical_margin))
        .saturating_sub(BRAILLE_ROW_GAPS.saturating_mul(grid.vertical_spacing))
        .saturating_sub(BRAILLE_DOT_ROWS.saturating_mul(grid.dot))
}

/// Fill every cell background, then overlay the selection beneath text.
fn background(
    drawing: &RowDrawing,
    metrics: &GridMetrics,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    window.paint_quad(fill(bounds, color(drawing.background)));
    for run in &drawing.runs {
        let background = if run.style.inverse {
            run.foreground
        } else {
            run.background
        };
        let bounds = cell_bounds(metrics, bounds.origin, run.column, run.columns);
        window.paint_quad(fill(bounds, color(background)));
    }
    if let Some(selected) = &drawing.selection {
        let mut tint: Hsla = rgb(SELECTION_COLOR).into();
        tint.a = SELECTION_OPACITY;
        window.paint_quad(fill(
            cell_bounds(
                metrics,
                bounds.origin,
                selected.start,
                selected.end.saturating_sub(selected.start),
            ),
            tint,
        ));
    }
}

/// Geometry shared by cell fills, selection and cursor painting.
pub(super) fn cell_bounds(
    metrics: &GridMetrics,
    origin: Point<Pixels>,
    column: u16,
    columns: u16,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            offset(origin.x, scale(metrics.cell_width, f32::from(column))),
            origin.y,
        ),
        size(
            scale(metrics.cell_width, f32::from(columns)),
            metrics.line_height,
        ),
    )
}

/// Draw the emulator's current cursor shape within its exact cell bounds.
fn cursor(drawing: &RowDrawing, metrics: &GridMetrics, origin: Point<Pixels>, window: &mut Window) {
    let Some(cursor) = &drawing.cursor else {
        return;
    };
    let mut bounds = cell_bounds(metrics, origin, cursor.column, cursor.columns);
    let mut tint = color(cursor.color);
    match cursor.style {
        CursorVisualStyle::Bar => bounds.size.width = px(STROKE_WIDTH),
        CursorVisualStyle::Underline => {
            bounds.origin.y = offset(
                bounds.origin.y,
                distance(bounds.size.height, px(STROKE_WIDTH)),
            );
            bounds.size.height = px(STROKE_WIDTH);
        }
        CursorVisualStyle::BlockHollow => {
            let stroke = px(STROKE_WIDTH);
            window.paint_quad(fill(
                Bounds::new(bounds.origin, size(bounds.size.width, stroke)),
                tint,
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x, distance(bounds.bottom(), stroke)),
                    size(bounds.size.width, stroke),
                ),
                tint,
            ));
            window.paint_quad(fill(
                Bounds::new(bounds.origin, size(stroke, bounds.size.height)),
                tint,
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(distance(bounds.right(), stroke), bounds.origin.y),
                    size(stroke, bounds.size.height),
                ),
                tint,
            ));
            return;
        }
        _ => tint.a = CURSOR_OPACITY,
    }
    window.paint_quad(fill(bounds, tint));
}

/// Paint cell-aligned decorations which the shaping API does not distinguish.
fn decorations(
    drawing: &RowDrawing,
    metrics: &GridMetrics,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    for run in drawing.runs.iter().filter(|run| !run.style.invisible) {
        let bounds = cell_bounds(metrics, origin, run.column, run.columns);
        if run.style.overline {
            let foreground = if run.style.inverse {
                run.background
            } else {
                run.foreground
            };
            let mut tint = color(foreground);
            if run.style.faint {
                tint.a *= FAINT_OPACITY;
            }
            window.paint_quad(fill(
                Bounds::new(bounds.origin, size(bounds.size.width, px(STROKE_WIDTH))),
                tint,
            ));
        }
        underline(run, metrics, bounds, window);
    }
}

/// Straight underline styles use terminal cells, preserving gaps and double rules.
fn underline(run: &CellRun, metrics: &GridMetrics, bounds: Bounds<Pixels>, window: &mut Window) {
    let origin = point(bounds.origin.x, distance(bounds.bottom(), px(STROKE_WIDTH)));
    let tint = color(run.underline);
    match run.style.underline {
        Underline::Single | Underline::Double => {
            window.paint_quad(fill(
                Bounds::new(origin, size(bounds.size.width, px(STROKE_WIDTH))),
                tint,
            ));
            if run.style.underline == Underline::Double {
                window.paint_quad(fill(
                    Bounds::new(
                        point(origin.x, distance(origin.y, px(DOUBLE_OFFSET))),
                        size(bounds.size.width, px(STROKE_WIDTH)),
                    ),
                    tint,
                ));
            }
        }
        Underline::Dotted | Underline::Dashed => {
            for column in 0..run.columns {
                let left = offset(origin.x, scale(metrics.cell_width, f32::from(column)));
                let width = if run.style.underline == Underline::Dotted {
                    px(STROKE_WIDTH)
                } else {
                    scale(metrics.cell_width, DASH_FRACTION)
                };
                window.paint_quad(fill(
                    Bounds::new(point(left, origin.y), size(width, px(STROKE_WIDTH))),
                    tint,
                ));
                if run.style.underline == Underline::Dotted {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(
                                offset(left, scale(metrics.cell_width, DOT_INTERVAL)),
                                origin.y,
                            ),
                            size(width, px(STROKE_WIDTH)),
                        ),
                        tint,
                    ));
                }
            }
        }
        _ => {}
    }
}
