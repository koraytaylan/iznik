//! Command shortcuts that move between the open tabs and the open sessions.
//!
//! Holding Command numbers the tabs of the session on screen, and Command
//! with a digit opens that tab. Holding Command and Option numbers the
//! sessions along the bottom, and Command-Option with a digit opens that
//! session. The arrow keys step through the same list: right and up move
//! forward, left and down move back, wrapping at either end.

use gpui_kit::{Context, KeyDownEvent, Modifiers, Window};

use crate::bars;
use crate::host_ui::EngineState;
use crate::window::{SessionKey, TabKey, WindowShell};

/// Which strip shows a shortcut number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutHint {
    /// Command is not held, so chips show their names alone.
    None,
    /// Command is held: number the tabs of the session on screen.
    Tabs,
    /// Command and Option are held: number the sessions.
    Sessions,
}

/// One Command shortcut: open a numbered place, or step to a neighbor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortcutStep {
    /// Open the entry at this 1-based place.
    Place(usize),
    /// Open the next entry when `forward`, otherwise the previous one.
    Step {
        /// Whether the step moves toward the end of the list.
        forward: bool,
    },
}

/// The digit keys that open a numbered tab or session, in order from one.
const DIGIT_KEYS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// The chip text, led by its 1-based place while `active` is `kind`.
#[must_use]
pub(crate) fn shown_chip(
    active: ShortcutHint,
    kind: ShortcutHint,
    place: usize,
    name: &str,
) -> String {
    if active == kind {
        format!("{place} {name}")
    } else {
        name.to_owned()
    }
}

/// Keep the shortcut numbers in step with Command being pressed or released.
pub(crate) fn on_modifiers(
    context: &Context<'_, WindowShell>,
) -> impl Fn(&gpui_kit::ModifiersChangedEvent, &mut Window, &mut gpui_kit::App) + 'static {
    context.listener(
        |shell, event: &gpui_kit::ModifiersChangedEvent, _window, context| {
            note(shell, event.modifiers, context);
        },
    )
}

/// Record which shortcut numbers the bars should show.
pub(crate) fn note(
    shell: &mut WindowShell,
    modifiers: Modifiers,
    context: &mut Context<'_, WindowShell>,
) {
    if shell.show_shortcut_hint(hint_of(modifiers)) {
        context.notify();
    }
}

/// Whether Command is held, alone or with Option, and no other modifier.
fn hint_of(modifiers: Modifiers) -> ShortcutHint {
    if !modifiers.platform || modifiers.control || modifiers.shift || modifiers.function {
        return ShortcutHint::None;
    }
    if modifiers.alt {
        ShortcutHint::Sessions
    } else {
        ShortcutHint::Tabs
    }
}

/// The shortcut a key asks for, when Command is held and the key is one.
fn step_of(key: &str) -> Option<ShortcutStep> {
    if let Some(index) = DIGIT_KEYS.iter().position(|digit| *digit == key) {
        return Some(ShortcutStep::Place(index.saturating_add(1)));
    }
    match key {
        "right" | "up" => Some(ShortcutStep::Step { forward: true }),
        "left" | "down" => Some(ShortcutStep::Step { forward: false }),
        _ => None,
    }
}

/// The tab at a 1-based place in the session on screen.
fn tab_at(state: &EngineState, selected: Option<&TabKey>, place: usize) -> Option<TabKey> {
    let index = place.checked_sub(1)?;
    bars::tab_keys(state, selected).get(index).cloned()
}

/// Every session chip, in the order the bottom bar draws them.
fn session_keys(state: &EngineState) -> Vec<SessionKey> {
    state
        .model()
        .hosts
        .iter()
        .flat_map(|(host, view)| {
            let alias = host.clone();
            view.model.sessions.iter().map(move |session| SessionKey {
                host: alias.clone(),
                session: session.id,
            })
        })
        .collect()
}

/// The session at a 1-based place along the bottom bar.
fn session_at(state: &EngineState, place: usize) -> Option<SessionKey> {
    let index = place.checked_sub(1)?;
    session_keys(state).get(index).cloned()
}

/// The next session in bar order, wrapping at either end.
fn next_session(
    state: &EngineState,
    selected: Option<&TabKey>,
    forward: bool,
) -> Option<SessionKey> {
    let keys = session_keys(state);
    let current = selected.and_then(|key| {
        keys.iter()
            .position(|candidate| candidate.host == key.host && candidate.session == key.session)
    });
    let index = current.unwrap_or(if forward {
        0
    } else {
        keys.len().saturating_sub(1)
    });
    keys.get(bars::next_index(index, keys.len(), forward))
        .cloned()
}

impl WindowShell {
    /// Command shortcuts that choose a tab or a session.
    ///
    /// Returns whether the key was one of those shortcuts, including when the
    /// numbered place does not exist. A digit opens that place. Right and up
    /// open the next entry, left and down the previous, wrapping at either
    /// end. Option chooses sessions; without it, the tabs on screen.
    pub(crate) fn shortcut_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> bool {
        let hint = hint_of(event.keystroke.modifiers);
        if hint == ShortcutHint::None {
            return false;
        }
        let Some(step) = step_of(&event.keystroke.key) else {
            return false;
        };
        match hint {
            ShortcutHint::Tabs => self.step_tabs(step, window, context),
            ShortcutHint::Sessions => self.step_sessions(step, window, context),
            ShortcutHint::None => return false,
        }
        true
    }

    /// Open a tab by number, or step to the next or previous tab.
    fn step_tabs(
        &mut self,
        step: ShortcutStep,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        match step {
            ShortcutStep::Step { forward } => {
                let _selected = self.select_next_tab(forward, window, context);
            }
            ShortcutStep::Place(place) => {
                let key = tab_at(self.hosts().state(), self.selected(), place);
                if let Some(key) = key {
                    let _selected = self.select(key, window, context);
                }
            }
        }
    }

    /// Open a session by number, or step to the next or previous session.
    fn step_sessions(
        &mut self,
        step: ShortcutStep,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        let key = match step {
            ShortcutStep::Place(place) => session_at(self.hosts().state(), place),
            ShortcutStep::Step { forward } => {
                next_session(self.hosts().state(), self.selected(), forward)
            }
        };
        if let Some(key) = key {
            let _selected = self.select_session(&key, window, context);
        }
    }
}
