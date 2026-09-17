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

use super::{CellRun, GridMetrics, RowDrawing, distance, offset, scale};

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
/// Byte position of red in an RGB integer.
const RED_SHIFT: u32 = 16;
/// Byte position of green in an RGB integer.
const GREEN_SHIFT: u32 = 8;

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
                let origin = point(
                    offset(
                        bounds.origin.x,
                        scale(self.metrics.cell_width, f32::from(glyph.column)),
                    ),
                    bounds.origin.y,
                );
                if let Err(error) = glyph.line.paint(
                    origin,
                    self.metrics.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    context,
                ) {
                    *self.error.borrow_mut() = Some(error.to_string());
                }
            }
            decorations(&self.drawing, &self.metrics, bounds.origin, window);
            cursor(&self.drawing, &self.metrics, bounds.origin, window);
        });
    }
}

/// Shape complete narrow runs so font ligatures remain intact. Wide graphemes
/// are separate runs, making their following column independent of fallback
/// font advances. GPUI's force-width mode is deliberately absent: it assigns
/// one column per glyph and would collapse multi-cell ligatures.
fn shape(drawing: &RowDrawing, metrics: &GridMetrics, window: &Window) -> Vec<GlyphRun> {
    drawing
        .runs
        .iter()
        .filter(|run| !run.style.invisible)
        .map(|run| {
            let mut selected_font = font(metrics.font.clone());
            if run.style.bold {
                selected_font = selected_font.bold();
            }
            if run.style.italic {
                selected_font = selected_font.italic();
            }
            let mut foreground = color(if run.style.inverse {
                run.background
            } else {
                run.foreground
            });
            if run.style.faint {
                foreground.a *= FAINT_OPACITY;
            }
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
