//! Command palette projection over the closed action inventory.

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Theme, WindowExt};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, Keystroke, ParentElement,
    ScrollHandle, StatefulInteractiveElement, Styled, TestSupportExt, WeakEntity, Window, div,
};
use iznik_client::commands::Submission;
use iznik_client::host::identity::HostId;
use iznik_protocol::command::SessionCommand;

use crate::actions::{ActionId, ActionSpec, INVENTORY, available};
use crate::host_ui::EngineState;
use crate::prompt::{self, Answer, HostOperation, Prompt, Step};
use crate::window::WindowShell;

/// Appended to an action's name when the model holds nothing it can act on.
const NOTHING_TO_ACT_ON: &str = "has nothing to act on";
/// Shown in the query while the search is empty.
const SEARCH_PLACEHOLDER: &str = "Type a command\u{2026}";
/// Shown in the query while an argument is empty.
const ANSWER_PLACEHOLDER: &str = "Type an answer\u{2026}";

/// The transient state of the command palette.
#[derive(Clone, Debug, Default)]
pub struct Palette {
    /// Whether the overlay is visible.
    pub open: bool,
    /// The text used to filter entries.
    pub query: String,
    /// The selected result position.
    pub selected: usize,
    /// The action waiting for its argument; while set, the query is the answer.
    pub prompt: Option<Prompt>,
    /// The scroll position of the result list, kept on the selected row.
    pub scroll: ScrollHandle,
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
        self.prompt = None;
    }

    /// Close the palette, abandoning any argument it was waiting for.
    pub fn close(&mut self) {
        self.open = false;
        self.prompt = None;
    }

    /// Turn the open palette into the argument step of an action.
    pub fn ask(&mut self, prompt: Prompt) {
        self.open = true;
        self.query.clone_from(&prompt.initial);
        self.selected = 0;
        self.prompt = Some(prompt);
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
        let action = self.key_action(key, character, result_count);
        self.scroll.scroll_to_item(self.selected);
        action
    }

    /// The state change and request of one normalized key.
    fn key_action(
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

/// How many rows the palette lists: choices while it asks, inventory otherwise.
#[must_use]
pub fn result_count(state: &EngineState, palette: &Palette) -> usize {
    palette.prompt.as_ref().map_or_else(
        || results(state, &palette.query).len(),
        |prompt| prompt::choices(prompt, &palette.query).len(),
    )
}

/// The empty list's words: what to type while a prompt takes an answer it did
/// not list, or that nothing matched.
#[must_use]
pub fn empty_hint(palette: &Palette) -> &'static str {
    palette
        .prompt
        .as_ref()
        .map_or("No command matches", |prompt| prompt::empty_hint(prompt))
}

/// One listed palette row: its element id, its text and what choosing it does.
struct Row {
    /// Stable element id.
    id: String,
    /// The name shown.
    text: String,
    /// The explanation shown below the name, for an inventory row.
    explanation: Option<&'static str>,
    /// The default chord shown at the row's end, when there is one.
    chord: Option<&'static str>,
    /// The inventory action to run, or `None` to answer the open prompt.
    action: Option<ActionId>,
}

/// The rows the palette lists in its current mode.
///
/// An Add Host prompt lists the ssh configuration's aliases and one row for
/// an answer that names none of them, so the palette is a chooser and a text
/// field at once.
fn rows(state: &EngineState, palette: &Palette) -> Vec<Row> {
    if let Some(prompt) = &palette.prompt {
        return prompt::choices(prompt, &palette.query)
            .into_iter()
            .enumerate()
            .map(|(index, choice)| Row {
                id: format!("prompt-choice-{index}"),
                text: choice.label,
                explanation: None,
                chord: None,
                action: None,
            })
            .collect();
    }
    results(state, &palette.query)
        .into_iter()
        .map(|specification| Row {
            id: format!("palette-{:?}", specification.id),
            text: specification.name.to_owned(),
            explanation: Some(specification.explanation),
            chord: specification.keybinding,
            action: Some(specification.id),
        })
        .collect()
}

/// One listed row: its name and explanation, its chord, highlighted while
/// selected, and choosing it on a click.
fn row_element(
    theme: &Theme,
    index: usize,
    row: Row,
    is_selected: bool,
    shell: Option<&WeakEntity<WindowShell>>,
) -> AnyElement {
    let row_background = if is_selected {
        theme.list_active
    } else {
        theme.popover
    };
    let mut label = div().flex().flex_col().flex_1().min_w_0().child(row.text);
    if let Some(explanation) = row.explanation {
        label = label.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(explanation),
        );
    }
    let mut element = div()
        .id(row.id)
        .test_support()
        .flex()
        .items_center()
        .gap_3()
        .mx_1()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(row_background)
        .text_color(theme.popover_foreground)
        .hover(|style| style.bg(theme.list_hover))
        .child(label);
    if let Some(chord) = row.chord {
        element = element.child(
            div()
                .flex_shrink_0()
                .px_1()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(chord),
        );
    }
    if let Some(target) = shell.cloned() {
        let action = row.action;
        element = element.on_click(move |_event, window, application| {
            let _ignored = target.update(application, |window_shell, context| {
                window_shell.palette_mut().selected = index;
                window_shell.choose(action, window, context);
            });
        });
    }
    element.into_any_element()
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
    let placeholder = if palette.prompt.is_some() {
        ANSWER_PLACEHOLDER
    } else {
        SEARCH_PLACEHOLDER
    };
    let query_text = if palette.query.is_empty() {
        placeholder.to_owned()
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
        .w_128()
        .max_h_128()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg();
    if let Some(prompt) = &palette.prompt {
        panel = panel.child(
            div()
                .id("command-palette-question")
                .test_support()
                .px_2()
                .pt_1()
                .text_color(theme.muted_foreground)
                .child(prompt.question.clone()),
        );
    }
    panel = panel.child(
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
    let mut list = div()
        .id("command-palette-rows")
        .flex()
        .flex_col()
        .min_h_0()
        .py_1()
        .overflow_y_scroll()
        .track_scroll(&palette.scroll);
    let rows = rows(state, palette);
    if rows.is_empty() {
        list = list.child(
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(empty_hint(palette)),
        );
    }
    for (index, row) in rows.into_iter().enumerate() {
        list = list.child(row_element(
            theme,
            index,
            row,
            index == palette.selected,
            shell,
        ));
    }
    let panel = panel.child(list);
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
/// An action that needs an argument turns the palette into its prompt
/// instead; an action with nothing to act on says so in a notification.
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
    match prompt::begin_with(
        action,
        shell.hosts().state(),
        shell.selected(),
        &shell.ssh_alias(),
    ) {
        Some(Step::Ask(prompt)) => {
            palette.ask(prompt);
            return Ok(true);
        }
        Some(Step::Perform(answer)) => {
            perform(shell, answer)?;
            palette.close();
            return Ok(true);
        }
        None => {}
    }
    let dispatched = shell.dispatch_action(action)?;
    if dispatched {
        palette.close();
    } else if let Some(specification) = INVENTORY
        .iter()
        .find(|specification| specification.id == action)
    {
        let message = format!("\u{201C}{}\u{201D} {NOTHING_TO_ACT_ON}", specification.name);
        window.push_notification(Notification::error(message), context);
    }
    Ok(dispatched)
}

/// Send the operation the open prompt's answer asks for, closing the palette
/// when the bridge accepts it.
///
/// An alias the ssh configuration does not define asks its follow-up question
/// for the address instead of sending anything; the palette stays open at that
/// question.
///
/// Returns `false` without sending anything while the answer is an empty
/// name or matches no choice.
///
/// # Errors
///
/// Returns the bridge error when the host is stopped or refuses the command.
pub fn submit_prompt(
    shell: &mut WindowShell,
    palette: &mut Palette,
) -> Result<bool, crate::bridge::EngineError> {
    let Some(answer) = palette
        .prompt
        .as_ref()
        .and_then(|prompt| prompt::answer(prompt, &palette.query, palette.selected))
    else {
        return Ok(false);
    };
    match answer {
        Answer::AddHost(alias) if !shell.ssh_defines(&alias) => {
            palette.ask(prompt::host_address_prompt(alias));
            Ok(true)
        }
        answer => {
            perform(shell, answer)?;
            palette.close();
            Ok(true)
        }
    }
}

/// Perform the operation an answer asks for through the shell's engine.
///
/// # Errors
///
/// Returns the bridge error when the host is stopped or refuses the operation,
/// and [`crate::ssh_config::SshConfigError`]'s words when the ssh
/// configuration cannot be written.
pub fn perform(shell: &mut WindowShell, answer: Answer) -> Result<(), crate::bridge::EngineError> {
    match answer {
        Answer::AddHost(alias) => shell.add_host(&alias),
        Answer::AddHostWithAddress { alias, address } => {
            shell.add_host_with_address(&alias, &address)
        }
        Answer::Host { operation, host } => match operation {
            HostOperation::Remove => shell.hosts_mut().remove_host(&host.0),
            HostOperation::Reconnect => shell.hosts_mut().reconnect(&host.0),
            // Forced, because a same-version server missing a capability is
            // exactly the case this exists for and nothing else will replace
            // it; the prompt has already said every session on the host ends.
            HostOperation::Upgrade => shell.hosts_mut().upgrade(&host.0, true),
            HostOperation::Uninstall => shell.hosts_mut().uninstall(&host.0),
        },
        Answer::Command { host, command } => shell.dispatch_command(&host.0, command).map(|_| ()),
    }
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
            let selected_action = if self.palette.prompt.is_some() {
                None
            } else {
                selected_action(self.hosts().state(), &self.palette)
            };
            self.choose(selected_action, window, context);
        }
        context.notify();
        action
    }
    /// Run an inventory action, or answer the open prompt when `action` is
    /// `None`, reporting a refusal as a failure notice.
    pub fn choose(
        &mut self,
        action: Option<ActionId>,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        let mut palette_state = std::mem::take(&mut self.palette);
        let result = match action {
            Some(action) => dispatch_action(self, &mut palette_state, action, window, context),
            None => submit_prompt(self, &mut palette_state),
        };
        self.palette = palette_state;
        if let Err(error) = result {
            self.failure(&HostId("palette".to_owned()), error.to_string(), context);
        }
        context.notify();
    }
    /// Run the action bound to a keystroke through the palette's own path: an
    /// argument-free action is sent at once, one that needs an argument opens
    /// the palette at its prompt. Returns whether a binding matched.
    pub fn chord(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> bool {
        let Some(action) = bound_action(&self.settings.keybindings, keystroke) else {
            return false;
        };
        self.palette.open();
        self.choose(Some(action), window, context);
        if self.palette.prompt.is_none() {
            self.palette.close();
        }
        true
    }
}

/// The available inventory action a keystroke is bound to: a settings
/// override by action name wins over the inventory's default chord.
fn bound_action(
    overrides: &std::collections::BTreeMap<String, String>,
    keystroke: &Keystroke,
) -> Option<ActionId> {
    INVENTORY
        .iter()
        .find(|specification| {
            overrides
                .get(&format!("{:?}", specification.id))
                .map(String::as_str)
                .or(specification.keybinding)
                .and_then(|chord| Keystroke::parse(chord).ok())
                .is_some_and(|bound| {
                    bound.modifiers == keystroke.modifiers && bound.key == keystroke.key
                })
        })
        .map(|specification| specification.id)
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
    let result_count = result_count(shell.hosts().state(), shell.palette());
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
pub(crate) fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut candidate_characters = candidate.chars();
    query.chars().all(|query_character| {
        candidate_characters.any(|candidate_character| candidate_character == query_character)
    })
}
