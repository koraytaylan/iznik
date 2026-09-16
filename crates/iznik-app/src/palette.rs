//! Command palette projection over the closed action inventory.

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Theme, WindowExt};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt, WeakEntity, Window, div,
};
use iznik_client::commands::Submission;
use iznik_client::host::identity::HostId;
use iznik_protocol::command::SessionCommand;

use crate::actions::{ActionId, ActionSpec, INVENTORY, available};
use crate::host_ui::EngineState;
use crate::window::WindowShell;

/// Shown when Add Host is chosen before its host-entry form exists.
const ADD_HOST_NOT_WIRED_UP: &str = "Adding a host isn't wired up yet.";

/// The transient state of the command palette.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    /// Whether the overlay is visible.
    pub open: bool,
    /// The text used to filter entries.
    pub query: String,
    /// The selected result position.
    pub selected: usize,
}

/// Result of handling one normalized palette key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    /// No command was selected.
    Ignored,
    /// The overlay should close.
    Closed,
    /// The currently selected inventory row should be dispatched.
    Dispatch,
}

impl Palette {
    /// Open the palette and clear its previous query and selection.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    /// Close the palette.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Replace the fuzzy query and return selection to the first result.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
    }

    /// Move the selection by one result, wrapping at either end.
    pub fn move_selection(&mut self, downward: bool, result_count: usize) {
        if result_count == 0 {
            self.selected = 0;
        } else if downward {
            self.selected = if self.selected == result_count.saturating_sub(1) {
                0
            } else {
                self.selected.saturating_add(1)
            };
        } else if self.selected == 0 {
            self.selected = result_count.saturating_sub(1);
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// Handle a normalized GPUI key and return the shell action it requests.
    #[must_use]
    pub fn key(
        &mut self,
        key: &str,
        character: Option<char>,
        result_count: usize,
    ) -> PaletteAction {
        match key {
            "escape" => {
                self.close();
                PaletteAction::Closed
            }
            "enter" => PaletteAction::Dispatch,
            "down" => {
                self.move_selection(true, result_count);
                PaletteAction::Ignored
            }
            "up" => {
                self.move_selection(false, result_count);
                PaletteAction::Ignored
            }
            "backspace" => {
                self.query.pop();
                self.selected = 0;
                PaletteAction::Ignored
            }
            _ => {
                if let Some(character) = character {
                    self.query.push(character);
                    self.selected = 0;
                }
                PaletteAction::Ignored
            }
        }
    }
}

/// Return available inventory rows whose names or explanations fuzzy-match a query.
#[must_use]
pub fn results<'state>(state: &'state EngineState, query: &str) -> Vec<&'state ActionSpec> {
    let normalized = query.to_lowercase();
    INVENTORY
        .iter()
        .filter(|specification| available(specification, state))
        .filter(|specification| {
            normalized.is_empty()
                || fuzzy_match(&specification.name.to_lowercase(), &normalized)
                || fuzzy_match(&specification.explanation.to_lowercase(), &normalized)
        })
        .collect()
}

/// Return the selected inventory identity after availability and fuzzy filtering.
#[must_use]
pub fn selected_action(state: &EngineState, palette: &Palette) -> Option<ActionId> {
    results(state, &palette.query)
        .get(palette.selected)
        .map(|specification| specification.id)
}

/// Render the dimmed, centered palette overlay from the inventory projection.
#[must_use]
pub fn render(
    theme: &Theme,
    state: &EngineState,
    palette: &Palette,
    shell: Option<&WeakEntity<WindowShell>>,
) -> AnyElement {
    let closed = div().id("command-palette").test_support();
    if !palette.open {
        return closed.into_any_element();
    }
    let query_text = if palette.query.is_empty() {
        "Type a command\u{2026}".to_owned()
    } else {
        palette.query.clone()
    };
    let query_color = if palette.query.is_empty() {
        theme.muted_foreground
    } else {
        theme.popover_foreground
    };
    let mut panel = div()
        .id("command-palette-panel")
        .test_support()
        .flex()
        .flex_col()
        .w_96()
        .max_h_96()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg()
        .child(
            div()
                .id("command-palette-query")
                .test_support()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .text_color(query_color)
                .child(query_text),
        );
    for (index, specification) in results(state, &palette.query).into_iter().enumerate() {
        let keybinding = specification.keybinding.unwrap_or("");
        let action = specification.id;
        let row_background = if index == palette.selected {
            theme.list_active
        } else {
            theme.popover
        };
        let mut row = div()
            .id(format!("palette-{:?}", specification.id))
            .test_support()
            .mx_1()
            .my_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(row_background)
            .text_color(theme.popover_foreground)
            .hover(|style| style.bg(theme.list_hover))
            .child(format!(
                "{} — {} {}",
                specification.name, specification.explanation, keybinding
            ));
        if let Some(target) = shell.cloned() {
            row = row.on_click(move |_event, window, application| {
                let _ignored = target.update(application, |window_shell, context| {
                    window_shell.palette_mut().selected = index;
                    let mut palette_state = std::mem::take(window_shell.palette_mut());
                    let _dispatched =
                        dispatch_action(window_shell, &mut palette_state, action, window, context);
                    *window_shell.palette_mut() = palette_state;
                    context.notify();
                });
            });
        }
        panel = panel.child(row);
    }
    closed
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.overlay)
        .child(panel)
        .into_any_element()
}

/// Dispatch a palette-selected session command through the shell bridge.
///
/// # Errors
///
/// Returns the same engine error as a keybinding or bar dispatch.
pub fn dispatch(
    shell: &mut WindowShell,
    palette: &mut Palette,
    alias: &str,
    command: SessionCommand,
) -> Result<Submission, crate::bridge::EngineError> {
    let result = shell.dispatch_command(alias, command);
    if result.is_ok() {
        palette.close();
    }
    result
}

/// Dispatch one inventory action and close the palette when the bridge accepts it.
///
/// # Errors
///
/// Returns the bridge error when the selected host is stopped or refuses the
/// command.
pub fn dispatch_action(
    shell: &mut WindowShell,
    palette: &mut Palette,
    action: ActionId,
    window: &mut Window,
    context: &mut Context<'_, WindowShell>,
) -> Result<bool, crate::bridge::EngineError> {
    if action == ActionId::OpenSettings {
        crate::settings_window::open(context);
        palette.close();
        return Ok(true);
    }
    let dispatched = shell.dispatch_action(action)?;
    if dispatched {
        palette.close();
    } else if action == ActionId::AddHost {
        window.push_notification(Notification::error(ADD_HOST_NOT_WIRED_UP), context);
    }
    Ok(dispatched)
}

impl WindowShell {
    /// Open the command palette and request a repaint.
    pub fn open_palette(&mut self, context: &mut Context<'_, Self>) {
        self.palette.open();
        context.notify();
    }
    /// Access the transient palette state for normalized input handlers.
    #[must_use]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }
    /// Mutably access the transient palette state for normalized input handlers.
    pub fn palette_mut(&mut self) -> &mut Palette {
        &mut self.palette
    }
    /// Route one normalized key into the open palette and repaint its overlay.
    #[must_use]
    pub fn palette_key(
        &mut self,
        key: &str,
        character: Option<char>,
        result_count: usize,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> PaletteAction {
        let action = self.palette.key(key, character, result_count);
        if matches!(action, PaletteAction::Dispatch) {
            let selected_action = selected_action(self.hosts().state(), &self.palette);
            if let Some(selected_action) = selected_action {
                let mut palette_state = std::mem::take(&mut self.palette);
                match dispatch_action(self, &mut palette_state, selected_action, window, context) {
                    Ok(true | false) => self.palette = palette_state,
                    Err(error) => {
                        self.palette = palette_state;
                        self.failure(&HostId("palette".to_owned()), error.to_string(), context);
                    }
                }
            }
        }
        context.notify();
        action
    }
}

/// Route one keyboard event into the palette: opening it on `ctrl-shift-p`
/// when closed, or forwarding its normalized key into the palette when open.
/// Returns whether the event was consumed and must not reach the tab bar or
/// terminal.
pub fn route_key(
    shell: &mut WindowShell,
    event: &KeyDownEvent,
    window: &mut Window,
    context: &mut Context<'_, WindowShell>,
) -> bool {
    let modifiers = &event.keystroke.modifiers;
    if !shell.palette().open {
        if modifiers.control && modifiers.shift && event.keystroke.key == "p" {
            shell.open_palette(context);
            return true;
        }
        return false;
    }
    let character = event
        .keystroke
        .key_char
        .as_deref()
        .and_then(|text| text.chars().next());
    let result_count = results(shell.hosts().state(), &shell.palette().query).len();
    let _action = shell.palette_key(
        &event.keystroke.key,
        character,
        result_count,
        window,
        context,
    );
    true
}

/// Whether the characters of a query occur in order in a candidate string.
fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut candidate_characters = candidate.chars();
    query.chars().all(|query_character| {
        candidate_characters.any(|candidate_character| candidate_character == query_character)
    })
}
