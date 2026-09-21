//! Map framework events into owned requests; the VT owner chooses terminal bytes.

use gpui_kit::{
    Context, DispatchPhase, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, Window, px,
};
use libghostty_vt::key::{Action, Key};
use libghostty_vt::terminal::ScrollViewport;

use super::keyboard::{keyboard, modifiers};
use super::{GridError, GridPosition, GridSelection, TerminalGrid, distance, scale};
use crate::input::{CopyInput, InputFrame, MouseInput, PointerAction, PointerInput, TerminalInput};

impl TerminalGrid {
    /// Leave composition to the platform and send each remaining press exactly once.
    pub(super) fn key_down(&mut self, event: &KeyDownEvent, context: &mut Context<'_, Self>) {
        if self.snapshot.is_none() || event.prefer_character_input || self.composition.active() {
            return;
        }
        if self.history_key(&event.keystroke, context) {
            context.stop_propagation();
            return;
        }
        let action = if event.is_held {
            Action::Repeat
        } else {
            Action::Press
        };
        let input = keyboard(&event.keystroke, action);
        if input.key == Key::Unidentified {
            return;
        }
        self.pressed_keys
            .insert(event.keystroke.key.clone(), input.clone());
        self.emit_input(TerminalInput::Key(input), context);
        context.stop_propagation();
    }

    /// Only presses delivered to the process can produce a protocol release.
    pub(super) fn key_up(&mut self, event: &KeyUpEvent, context: &mut Context<'_, Self>) {
        if let Some(mut input) = self.pressed_keys.remove(&event.keystroke.key) {
            input.action = Action::Release;
            input.modifiers = modifiers(event.keystroke.modifiers);
            input.text.clear();
            self.emit_input(TerminalInput::Key(input), context);
            context.stop_propagation();
        }
    }

    /// Shift-only history chords stay local; additional modifiers belong to the process.
    fn history_key(&self, stroke: &Keystroke, context: &mut Context<'_, Self>) -> bool {
        let modifiers = stroke.modifiers;
        if !modifiers.shift
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.function
        {
            return false;
        }
        let rows = self.snapshot.as_ref().map_or(1, |snapshot| {
            isize::try_from(snapshot.rows.len()).unwrap_or(isize::MAX)
        });
        let scroll = match stroke.key.as_str() {
            "pageup" => ScrollViewport::Delta(rows.saturating_neg()),
            "pagedown" => ScrollViewport::Delta(rows),
            "home" => ScrollViewport::Top,
            "end" => ScrollViewport::Bottom,
            _ => return false,
        };
        self.scroll(scroll, context);
        true
    }
}

impl TerminalGrid {
    /// Apply a local gesture returned by the VT owner only if its displayed frame
    /// still matches. The window routes `LocalPointer` here instead of engine input.
    ///
    /// # Errors
    /// Returns invalid selection geometry from the held snapshot.
    pub fn apply_pointer(
        &mut self,
        input: &PointerInput,
        context: &mut Context<'_, Self>,
    ) -> Result<bool, GridError> {
        let Some(snapshot) = &self.snapshot else {
            return Ok(false);
        };
        if snapshot.key != input.frame.key
            || (matches!(input.local, Some(PointerAction::Select(_)))
                && InputFrame::from(snapshot) != input.frame)
        {
            self.selection_anchor = None;
            return Ok(false);
        }
        match input.local {
            Some(PointerAction::Select(position)) => {
                match input.mouse.action {
                    libghostty_vt::mouse::Action::Press => self.selection_anchor = Some(position),
                    libghostty_vt::mouse::Action::Motion
                    | libghostty_vt::mouse::Action::Release => {}
                    _ => return Ok(false),
                }
                if let Some(anchor) = self.selection_anchor {
                    let mut start = anchor;
                    let mut end = position;
                    let click = input.mouse.action == libghostty_vt::mouse::Action::Release
                        && position == anchor
                        && self
                            .selection
                            .is_some_and(|selection| selection.anchor == selection.head);
                    if input.mouse.action != libghostty_vt::mouse::Action::Press && !click {
                        if end >= start {
                            end.column = end.column.saturating_add(1);
                        } else {
                            start.column = start.column.saturating_add(1);
                        }
                    }
                    self.select(
                        Some(GridSelection {
                            anchor: start,
                            head: end,
                        }),
                        context,
                    )?;
                }
                if input.mouse.action == libghostty_vt::mouse::Action::Release {
                    self.selection_anchor = None;
                }
            }
            Some(PointerAction::Scroll(rows)) => self.scroll(ScrollViewport::Delta(rows), context),
            None => return Ok(false),
        }
        Ok(true)
    }

    /// Request native plain-text serialization of the currently displayed selection.
    pub fn copy_selection(&self, context: &mut Context<'_, Self>) {
        if let (Some(selection), Some(snapshot)) = (self.selection, &self.snapshot) {
            self.emit_input(
                TerminalInput::Copy(CopyInput {
                    selection,
                    frame: InputFrame::from(snapshot),
                }),
                context,
            );
        }
    }

    /// Send text to the process through the native paste encoder, which frames
    /// it and strips the control bytes that would escape a bracketed payload.
    pub fn paste_text(&self, text: String, context: &mut Context<'_, Self>) {
        self.emit_input(TerminalInput::Paste(text), context);
    }
}

/// Capture only an owned drag outside the surface; hovered motion uses the div listener.
/// This preserves selection and program mouse releases when the pointer leaves a pane.
pub(super) fn track_drag(entity: &Entity<TerminalGrid>, window: &mut Window) {
    let entity = entity.downgrade();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, application| {
        if phase != DispatchPhase::Capture {
            return;
        }
        let _updated = entity.update(application, |grid, context| {
            if grid.pointer_pressed.is_some()
                && grid
                    .bounds
                    .is_some_and(|bounds| !bounds.contains(&event.position))
            {
                grid.pointer_move(event, window, context);
            }
        });
    });
}

/// Round physical pixel dimensions to the integer geometry the native encoder accepts.
fn native_pixels(value: Pixels) -> u32 {
    u32::try_from(usize::from(px(f32::from(value).round().max(1.0)))).unwrap_or(u32::MAX)
}

/// Map the three ordinary pointer buttons without inventing navigation-button meanings.
fn mouse_button(button: MouseButton) -> Option<libghostty_vt::mouse::Button> {
    use libghostty_vt::mouse::Button;
    match button {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        MouseButton::Right => Some(Button::Right),
        MouseButton::Navigate(_) => None,
    }
}

impl TerminalGrid {
    /// Capture the originating pane and Shift override before queuing the press.
    pub(super) fn pointer_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        if self.snapshot.is_none() {
            return;
        }
        window.focus(&self.focus, context);
        self.pointer_pressed = Some(event.button);
        self.pointer_override = event.modifiers.shift;
        self.pointer(
            event.position,
            event.modifiers,
            libghostty_vt::mouse::Action::Press,
            window,
            context,
        );
    }

    /// Keep the original button and override across a drag, including outside bounds.
    pub(super) fn pointer_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        context: &mut Context<'_, Self>,
    ) {
        self.pointer(
            event.position,
            event.modifiers,
            libghostty_vt::mouse::Action::Motion,
            window,
            context,
        );
    }

    /// Pair releases with presses from this pane, even if Shift was released first.
    pub(super) fn pointer_up(
        &mut self,
        event: &MouseUpEvent,
        window: &Window,
        context: &mut Context<'_, Self>,
    ) {
        if self.pointer_pressed != Some(event.button) {
            return;
        }
        self.pointer(
            event.position,
            event.modifiers,
            libghostty_vt::mouse::Action::Release,
            window,
            context,
        );
        self.pointer_pressed = None;
        self.pointer_override = false;
    }

    /// Clamp local selection to displayed cells while native reporting receives surface pixels.
    fn pointer_cell(&self, position: Point<Pixels>) -> Option<GridPosition> {
        let snapshot = self.snapshot.as_ref()?;
        let bounds = self.bounds?;
        let column = usize::from(px(f32::from(
            distance(position.x, bounds.origin.x).max(px(0.0)),
        ) / f32::from(self.metrics.cell_width)));
        let row = usize::from(px(f32::from(
            distance(position.y, bounds.origin.y).max(px(0.0)),
        ) / f32::from(self.metrics.line_height)));
        Some(GridPosition {
            column: u16::try_from(column)
                .unwrap_or(u16::MAX)
                .min(snapshot.columns.saturating_sub(1)),
            row: u16::try_from(row.min(snapshot.rows.len().saturating_sub(1))).unwrap_or(u16::MAX),
        })
    }

    /// Build physical native geometry and a frame-bound local selection fallback.
    fn pointer_input(
        &self,
        position: Point<Pixels>,
        held: Modifiers,
        action: libghostty_vt::mouse::Action,
        window: &Window,
    ) -> Option<PointerInput> {
        let snapshot = self.snapshot.as_ref()?;
        let bounds = self.bounds?;
        let local = if self.pointer_pressed == Some(MouseButton::Left) {
            self.pointer_cell(position).map(PointerAction::Select)
        } else {
            None
        };
        let factor = window.scale_factor();
        Some(PointerInput {
            frame: InputFrame::from(snapshot),
            local,
            mouse: MouseInput {
                action,
                button: self.pointer_pressed.and_then(mouse_button),
                modifiers: modifiers(held),
                position: libghostty_vt::mouse::Position {
                    x: f32::from(scale(distance(position.x, bounds.origin.x), factor)),
                    y: f32::from(scale(distance(position.y, bounds.origin.y), factor)),
                },
                geometry: libghostty_vt::mouse::EncoderSize {
                    screen_width: native_pixels(scale(bounds.size.width, factor)),
                    screen_height: native_pixels(scale(bounds.size.height, factor)),
                    cell_width: native_pixels(scale(self.metrics.cell_width, factor)),
                    cell_height: native_pixels(scale(self.metrics.line_height, factor)),
                    padding_top: 0,
                    padding_bottom: 0,
                    padding_left: 0,
                    padding_right: 0,
                },
                pressed: self.pointer_pressed.is_some(),
            },
        })
    }

    /// Dispatch a press, move or release through shared geometry and the retained override.
    fn pointer(
        &mut self,
        position: Point<Pixels>,
        held: Modifiers,
        action: libghostty_vt::mouse::Action,
        window: &Window,
        context: &mut Context<'_, Self>,
    ) {
        if let Some(input) = self.pointer_input(position, held, action, window) {
            self.dispatch_pointer(input, self.pointer_override, context);
        }
    }

    /// Preserve smooth wheel accumulation and let native tracking select program or history.
    pub(super) fn pointer_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &Window,
        context: &mut Context<'_, Self>,
    ) {
        let Some(rows) = self.wheel_rows(event.delta) else {
            return;
        };
        let Some(mut input) = self.pointer_input(
            event.position,
            event.modifiers,
            libghostty_vt::mouse::Action::Press,
            window,
        ) else {
            return;
        };
        input.mouse.button = Some(if rows < 0 {
            libghostty_vt::mouse::Button::Four
        } else {
            libghostty_vt::mouse::Button::Five
        });
        input.local = Some(PointerAction::Scroll(rows));
        self.dispatch_pointer(input, event.modifiers.shift, context);
    }

    /// Queue native mode resolution, except for an explicit local Shift override.
    fn dispatch_pointer(
        &mut self,
        input: PointerInput,
        local: bool,
        context: &mut Context<'_, Self>,
    ) {
        if local {
            if let Err(error) = self.apply_pointer(&input, context) {
                self.interaction_error = Some(error.to_string());
            }
        } else {
            self.emit_input(TerminalInput::Pointer(input), context);
        }
        context.stop_propagation();
    }
}
