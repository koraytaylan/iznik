//! Command palette projection over the closed action inventory.

use gpui_kit::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    TestSupportExt, WeakEntity, div,
};
use iznik_client::commands::Submission;
use iznik_protocol::command::SessionCommand;

use crate::actions::{ActionId, ActionSpec, INVENTORY, available};
use crate::host_ui::EngineState;
use crate::window::WindowShell;

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
    state: &EngineState,
    palette: &Palette,
    shell: Option<&WeakEntity<WindowShell>>,
) -> AnyElement {
    let mut overlay = div().id("command-palette").test_support().flex().flex_col();
    if palette.open {
        for (index, specification) in results(state, &palette.query).into_iter().enumerate() {
            let keybinding = specification.keybinding.unwrap_or("");
            let action = specification.id;
            let mut row = div()
                .id(format!("palette-{:?}", specification.id))
                .test_support()
                .child(format!(
                    "{} — {} {}",
                    specification.name, specification.explanation, keybinding
                ));
            if let Some(target) = shell.cloned() {
                row = row.on_click(move |_event, _window, application| {
                    let _ignored = target.update(application, |window_shell, context| {
                        window_shell.palette_mut().selected = index;
                        let mut palette_state = std::mem::take(window_shell.palette_mut());
                        let _dispatched = dispatch_action(window_shell, &mut palette_state, action);
                        *window_shell.palette_mut() = palette_state;
                        context.notify();
                    });
                });
            }
            overlay = overlay.child(row);
        }
    }
    overlay.into_any_element()
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
) -> Result<bool, crate::bridge::EngineError> {
    let dispatched = shell.dispatch_action(action)?;
    if dispatched {
        palette.close();
    }
    Ok(dispatched)
}

/// Whether the characters of a query occur in order in a candidate string.
fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut candidate_characters = candidate.chars();
    query.chars().all(|query_character| {
        candidate_characters.any(|candidate_character| candidate_character == query_character)
    })
}
