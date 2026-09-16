//! Platform composition edits only an unsent draft, never remote terminal output.

use std::ops::Range;

use gpui_kit::{
    Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler, IntoElement,
    Pixels, Point, ShapedLine, Styled, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    canvas, fill, font, point, px, size,
};
use libghostty_vt::key::{Action, Key, Mods};

use super::{GridInput, TerminalGrid, distance, offset, paint::color, scale};
use crate::input::{KeyInput, TerminalInput};

/// One logical pixel keeps an empty caret usable as a platform candidate anchor.
const CARET_WIDTH: f32 = 1.0;

/// Unsent preedit state; all exposed ranges count UTF-16 code units.
#[derive(Debug, Default)]
pub(super) struct Composition {
    /// Text owned by the input method until committed or cancelled.
    text: String,
    /// Selection within the draft, normalized to whole Unicode scalar boundaries.
    selected: Range<usize>,
    /// Painting failure retained for the existing surface diagnostics.
    pub(super) error: Option<String>,
}

/// Convert a UTF-16 offset to the preceding complete UTF-8 boundary.
fn byte_offset(text: &str, offset: usize) -> usize {
    let mut units = 0_usize;
    for (index, character) in text.char_indices() {
        let end = units.saturating_add(character.len_utf16());
        if end > offset {
            return index;
        }
        units = end;
    }
    text.len()
}

/// Clamp unordered platform ranges to complete characters without slicing a surrogate.
fn byte_range(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset(text, range.start.min(range.end))..byte_offset(text, range.start.max(range.end))
}

/// Count UTF-16 units before an already normalized byte offset.
fn units_before(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(0, |prefix| prefix.encode_utf16().count())
}

impl Composition {
    /// Let the platform finish a draft before raw terminal key dispatch resumes.
    pub(super) fn active(&self) -> bool {
        !self.text.is_empty()
    }

    /// Normalize the current selection after an input method supplied a range.
    fn select(&mut self, range: Range<usize>) {
        let bytes = byte_range(&self.text, range);
        self.selected = units_before(&self.text, bytes.start)..units_before(&self.text, bytes.end);
    }

    /// Replace the requested draft range, defaulting to the complete marked text.
    fn replace(&mut self, range: Option<Range<usize>>, text: &str) -> usize {
        let range = range.map_or(0..self.text.len(), |range| byte_range(&self.text, range));
        let start = units_before(&self.text, range.start);
        self.text.replace_range(range, text);
        let end = start.saturating_add(text.encode_utf16().count());
        self.selected = end..end;
        start
    }
}

/// Register the focused platform handler and paint preedit above cached terminal rows.
pub(super) fn overlay(entity: Entity<TerminalGrid>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, application| {
            super::interaction::track_drag(&entity, window);
            let focus = entity.read(application).focus.clone();
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, entity.clone()),
                application,
            );
            entity.update(application, |grid, context| {
                grid.bounds = Some(bounds);
                grid.composition.error = None;
                if grid.composition.text.is_empty() {
                    return;
                }
                let Some(origin) = grid.composition_origin(bounds) else {
                    return;
                };
                let Some(line) = grid.composition_line(window) else {
                    return;
                };
                let Some(snapshot) = &grid.snapshot else {
                    return;
                };
                window.paint_quad(fill(
                    Bounds::new(origin, size(line.width, grid.metrics.line_height)),
                    color(snapshot.colors.background),
                ));
                if let Err(error) = line.paint(
                    origin,
                    grid.metrics.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    context,
                ) {
                    grid.composition.error = Some(error.to_string());
                }
            });
        },
    )
    .absolute()
    .size_full()
}

impl TerminalGrid {
    /// Send text through the VT owner's native key encoder, including extended modes.
    fn commit_composition(&mut self, context: &mut Context<'_, Self>) {
        let text = std::mem::take(&mut self.composition.text);
        self.composition.selected = 0..0;
        if !text.is_empty() {
            self.emit_input(
                TerminalInput::Key(KeyInput {
                    key: Key::Unidentified,
                    action: Action::Press,
                    modifiers: Mods::empty(),
                    consumed: Mods::empty(),
                    text,
                    unshifted: None,
                }),
                context,
            );
        }
        context.notify();
    }

    /// Route owned input only after the surface has an authoritative pane identity.
    pub(super) fn emit_input(&self, input: TerminalInput, context: &mut Context<'_, Self>) {
        if let Some(snapshot) = &self.snapshot {
            context.emit(GridInput {
                key: snapshot.key.clone(),
                input,
            });
        }
    }

    /// Position the draft at the emulator cursor in the current viewport.
    fn composition_origin(&self, bounds: Bounds<Pixels>) -> Option<Point<Pixels>> {
        let cursor = self.snapshot.as_ref()?.cursor?;
        Some(point(
            offset(
                bounds.origin.x,
                scale(self.metrics.cell_width, f32::from(cursor.x)),
            ),
            offset(
                bounds.origin.y,
                scale(self.metrics.line_height, f32::from(cursor.y)),
            ),
        ))
    }

    /// Use the same font shaping for painting, hit testing and candidate geometry.
    fn composition_line(&self, window: &Window) -> Option<ShapedLine> {
        let snapshot = self.snapshot.as_ref()?;
        let foreground = color(snapshot.colors.foreground);
        Some(window.text_system().shape_line(
            self.composition.text.clone().into(),
            self.metrics.font_size,
            &[TextRun {
                len: self.composition.text.len(),
                font: font(self.metrics.font.clone()),
                color: foreground,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(CARET_WIDTH),
                    color: Some(foreground),
                    wavy: false,
                }),
                strikethrough: None,
            }],
            None,
        ))
    }
}

impl EntityInputHandler for TerminalGrid {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<String> {
        let bytes = byte_range(&self.composition.text, range);
        *adjusted_range = Some(
            units_before(&self.composition.text, bytes.start)
                ..units_before(&self.composition.text, bytes.end),
        );
        self.composition.text.get(bytes).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition.selected.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<Range<usize>> {
        (!self.composition.text.is_empty()).then(|| 0..self.composition.text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, context: &mut Context<'_, Self>) {
        self.commit_composition(context);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        self.composition.replace(range, text);
        self.commit_composition(context);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        let start = self.composition.replace(range, new_text);
        if let Some(selected) = new_selected_range {
            self.composition
                .select(start.saturating_add(selected.start)..start.saturating_add(selected.end));
        }
        context.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        let origin = self.composition_origin(element_bounds)?;
        let bytes = byte_range(&self.composition.text, range_utf16);
        let line = self.composition_line(window)?;
        let left = line.x_for_index(bytes.start);
        let right = line.x_for_index(bytes.end);
        Some(Bounds::new(
            point(offset(origin.x, left), origin.y),
            size(
                distance(right, left).max(px(CARET_WIDTH)),
                self.metrics.line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<usize> {
        let origin = self.composition_origin(self.bounds?)?;
        let line = self.composition_line(window)?;
        let index = line.closest_index_for_x(distance(point.x, origin.x));
        Some(units_before(&self.composition.text, index))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        self.composition.select(range_utf16);
        context.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<usize> {
        Some(self.composition.text.encode_utf16().count())
    }

    fn text_input_editable_range(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> Option<Range<usize>> {
        Some(0..self.composition.text.encode_utf16().count())
    }

    fn paste(
        &mut self,
        item: ClipboardItem,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        self.commit_composition(context);
        if let Some(text) = item.text() {
            self.emit_input(TerminalInput::Paste(text), context);
        }
    }
}
