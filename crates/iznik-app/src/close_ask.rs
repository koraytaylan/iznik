//! Whether closing a tab or a session asks first, and the dialog that asks.
//!
//! The host samples each pane's foreground program about once a second and
//! publishes a program name as the pane title. A shell, or a title that is
//! only a path, is idle. Anything else is a program that closing would end.
//! A session with more than one tab asks even when every pane is idle,
//! because the whole session goes. Each question is its own setting, on
//! unless that setting is turned off.
//!
//! The dialog names the close, says what ends, and offers that close beside
//! cancel. Return confirms. Escape cancels, and so does a click on the dimmed
//! window. Nothing is sent until the close is confirmed.

use gpui_kit::Hsla;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Role,
    StatefulInteractiveElement as _, Styled, TestSupportExt, div,
};
use iznik_client::host::identity::HostId;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{SessionId, TabId};
use iznik_protocol::model::{HostModel, Pane, Session, Tab};
use iznik_protocol::program::program_label;

use crate::actions::ActionId;
use crate::host_ui::EngineState;
use crate::prompt::Answer;
use crate::window::{TabKey, WindowShell};

/// The label of the button that leaves the close unsent.
pub const CANCEL_LABEL: &str = "Cancel";

/// How the dialog is answered from the keyboard, or by clicking the dimmed window.
pub const DISMISS_HINT: &str = "Return closes. Escape cancels, and so does a click outside.";

/// Which closes ask first. Both are on unless a setting turns one off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseAsks {
    /// Ask before closing a session that holds more than one tab.
    pub session: bool,
    /// Ask before closing a tab or session that is running a program.
    pub running: bool,
}

impl Default for CloseAsks {
    fn default() -> Self {
        Self {
            session: true,
            running: true,
        }
    }
}

/// What the close dialog says, and the commands confirming it sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseQuestion {
    /// The short question in the dialog's title.
    pub title: String,
    /// What the close ends, under the title.
    pub detail: String,
    /// The label of the button that closes.
    pub confirm: String,
    /// What confirming sends.
    pub answer: Answer,
}

/// The close question for one palette action, using [`CloseAsks`].
///
/// `None` when the action is not a close, or when this close needs no question.
#[must_use]
pub fn step(
    action: ActionId,
    state: &EngineState,
    selected: &TabKey,
    asks: CloseAsks,
) -> Option<CloseQuestion> {
    let command = match action {
        ActionId::CloseTab => SessionCommand::CloseTab { tab: selected.tab },
        ActionId::CloseSession => SessionCommand::CloseSession {
            session: selected.session,
        },
        _ => return None,
    };
    question(state, &selected.host, std::slice::from_ref(&command), asks)
}

/// The question for these close commands, or `None` when they close at once.
///
/// A list that is empty or holds anything other than a close does not ask:
/// the caller sends it as it is.
#[must_use]
pub fn question(
    state: &EngineState,
    host: &HostId,
    commands: &[SessionCommand],
    asks: CloseAsks,
) -> Option<CloseQuestion> {
    let gathered = gather(state, host, commands)?;
    if !needs_question(&gathered, asks) {
        return None;
    }
    Some(build(host, commands, &gathered))
}

/// Colours copied off the theme before the dialog borrows the window.
struct DialogColors {
    /// The dimmed window behind the card.
    overlay: Hsla,
    /// The card's border.
    border: Hsla,
    /// The card's field.
    field: Hsla,
    /// The card's text.
    foreground: Hsla,
    /// The explanation and the hint.
    quiet: Hsla,
}

/// The close dialog for this window, or an empty element while none is waiting.
#[must_use]
pub fn overlay(shell: &WindowShell, context: &mut Context<'_, WindowShell>) -> AnyElement {
    render(
        shell
            .following
            .close_question
            .as_ref()
            .map(|open| &open.question),
        context,
    )
}

/// The dialog, or an empty element while no close is waiting.
#[must_use]
fn render(question: Option<&CloseQuestion>, context: &mut Context<'_, WindowShell>) -> AnyElement {
    let colors = DialogColors {
        overlay: context.theme().overlay,
        border: context.theme().border,
        field: context.theme().popover,
        foreground: context.theme().popover_foreground,
        quiet: context.theme().muted_foreground,
    };
    let mut overlay = div().id("close-confirmation").test_support();
    let Some(question) = question else {
        return overlay.into_any_element();
    };
    overlay = overlay
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(colors.overlay)
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            context.listener(|shell, _, _, context| {
                shell.dismiss_close(context);
            }),
        )
        .child(card(&colors, question, context));
    overlay.into_any_element()
}

/// What the close commands would end, read from the model.
struct Gathered {
    /// Close-tab commands in the list.
    tab_closes: usize,
    /// Sessions the close-session commands name.
    sessions: Vec<SessionId>,
    /// The one session's name, when exactly one session close names a session
    /// the model still holds.
    session_name: Option<String>,
    /// Tabs the session closes would take with them.
    tab_total: usize,
    /// Whether any session being closed holds more than one tab.
    many_tabs: bool,
    /// Programs other than a shell that the closes would end, once each.
    programs: Vec<String>,
}

/// Read the closes, or `None` when the list is not only closes.
fn gather(state: &EngineState, host: &HostId, commands: &[SessionCommand]) -> Option<Gathered> {
    if commands.is_empty() {
        return None;
    }
    let model = state.model().host(host).map(|view| &view.model);
    let mut gathered = Gathered {
        tab_closes: 0,
        sessions: Vec::new(),
        session_name: None,
        tab_total: 0,
        many_tabs: false,
        programs: Vec::new(),
    };
    for command in commands {
        match command {
            SessionCommand::CloseTab { tab } => note_tab(&mut gathered, model, *tab),
            SessionCommand::CloseSession { session } => {
                note_session(&mut gathered, model, *session);
            }
            _ => return None,
        }
    }
    if gathered.sessions.len() == 1 {
        let session = gathered.sessions.first().copied();
        gathered.session_name =
            session.and_then(|id| model.and_then(|held| session_named(held, id)));
    }
    Some(gathered)
}

/// Record one tab close and the programs its panes are running.
fn note_tab(gathered: &mut Gathered, model: Option<&HostModel>, tab: TabId) {
    gathered.tab_closes = gathered.tab_closes.saturating_add(1);
    if let Some(found) = model.and_then(|held| tab_in(held, tab)) {
        remember_programs(&mut gathered.programs, &found.panes);
    }
}

/// Record one session close, its tabs and the programs its panes are running.
fn note_session(gathered: &mut Gathered, model: Option<&HostModel>, session: SessionId) {
    gathered.sessions.push(session);
    let Some(found) = model.and_then(|held| session_in(held, session)) else {
        return;
    };
    if found.tabs.len() > 1 {
        gathered.many_tabs = true;
    }
    gathered.tab_total = gathered.tab_total.saturating_add(found.tabs.len());
    for tab in &found.tabs {
        remember_programs(&mut gathered.programs, &tab.panes);
    }
}

/// Whether `asks` wants a question for what `gathered` would end.
fn needs_question(gathered: &Gathered, asks: CloseAsks) -> bool {
    let running = asks.running && !gathered.programs.is_empty();
    let session = asks.session && gathered.many_tabs;
    running || session
}

/// The dialog copy whose confirm button sends `commands`.
fn build(host: &HostId, commands: &[SessionCommand], gathered: &Gathered) -> CloseQuestion {
    let (title, detail, confirm) = close_text(gathered);
    CloseQuestion {
        title,
        detail,
        confirm,
        answer: answer_for(host, commands),
    }
}

/// The title, the explanation and the confirm label for what is being closed.
fn close_text(gathered: &Gathered) -> (String, String, String) {
    if gathered.sessions.is_empty() {
        return tab_text(gathered.tab_closes, &gathered.programs);
    }
    session_text(gathered)
}

/// The title, explanation and confirm label for closing tabs.
fn tab_text(count: usize, programs: &[String]) -> (String, String, String) {
    let running = programs_text(programs);
    let ends = ends_word(programs);
    let confirm = if count > 1 {
        format!("Close {count} Tabs")
    } else {
        "Close Tab".to_owned()
    };
    if count > 1 {
        (
            format!("Close {count} tabs?"),
            format!("{running}. Closing these tabs ends {ends}."),
            confirm,
        )
    } else {
        (
            "Close this tab?".to_owned(),
            format!("{running} in this tab. Closing the tab ends {ends}."),
            confirm,
        )
    }
}

/// The title, explanation and confirm label for closing sessions.
fn session_text(gathered: &Gathered) -> (String, String, String) {
    let count = gathered.sessions.len();
    let confirm = if count > 1 {
        format!("Close {count} Sessions")
    } else {
        "Close Session".to_owned()
    };
    if count > 1 {
        return (
            format!("Close {count} sessions?"),
            many_sessions(gathered),
            confirm,
        );
    }
    let name = gathered
        .session_name
        .clone()
        .unwrap_or_else(|| "this session".to_owned());
    (
        format!("Close \u{201C}{name}\u{201D}?"),
        one_session(gathered),
        confirm,
    )
}

/// The explanation for closing more than one session.
fn many_sessions(gathered: &Gathered) -> String {
    let tabs = gathered.tab_total;
    if gathered.programs.is_empty() {
        return format!(
            "These sessions hold {tabs} tabs. Closing them closes every tab. The shells are idle."
        );
    }
    let running = programs_text(&gathered.programs);
    let ends = ends_word(&gathered.programs);
    format!("These sessions hold {tabs} tabs. {running}, and closing the sessions ends {ends}.")
}

/// The explanation for closing one session.
fn one_session(gathered: &Gathered) -> String {
    let tabs = gathered.tab_total;
    if gathered.programs.is_empty() {
        return format!(
            "This session has {tabs} tabs. Closing it closes all of them. The shells are idle."
        );
    }
    let running = programs_text(&gathered.programs);
    let ends = ends_word(&gathered.programs);
    if gathered.many_tabs {
        format!("This session has {tabs} tabs. {running}, and closing the session ends {ends}.")
    } else {
        format!("{running} in this session. Closing the session ends {ends}.")
    }
}

/// `programs` as a clause: "vim is running", or "vim and htop are running".
fn programs_text(programs: &[String]) -> String {
    let names = listed(programs);
    if programs.len() > 1 {
        format!("{names} are running")
    } else {
        format!("{names} is running")
    }
}

/// The pronoun for the programs a close would end.
fn ends_word(programs: &[String]) -> &'static str {
    if programs.len() > 1 { "them" } else { "it" }
}

/// One command, or every command when the close is a batch.
fn answer_for(host: &HostId, commands: &[SessionCommand]) -> Answer {
    match commands {
        [command] => Answer::Command {
            host: host.clone(),
            command: command.clone(),
        },
        _ => Answer::Commands {
            host: host.clone(),
            commands: commands.to_vec(),
        },
    }
}

/// The session's name, when the model holds it.
fn session_named(model: &HostModel, session: SessionId) -> Option<String> {
    session_in(model, session).map(|found| found.name.clone())
}

/// The session, when the model holds it.
fn session_in(model: &HostModel, session: SessionId) -> Option<&Session> {
    model
        .sessions
        .iter()
        .find(|candidate| candidate.id == session)
}

/// The tab, when the model holds it.
fn tab_in(model: &HostModel, tab: TabId) -> Option<&Tab> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .find(|candidate| candidate.id == tab)
}

/// Append program names the panes are running, skipping a shell and a repeat.
fn remember_programs(programs: &mut Vec<String>, panes: &[Pane]) {
    for pane in panes {
        let Some(label) = program_label(&pane.title) else {
            continue;
        };
        if programs.iter().any(|held| held == label) {
            continue;
        }
        programs.push(label.to_owned());
    }
}

/// `names` as a sentence list: one name, two joined with "and", or the last
/// set off with "and".
fn listed(names: &[String]) -> String {
    let Some((last, head)) = names.split_last() else {
        return String::new();
    };
    if head.is_empty() {
        return last.clone();
    }
    let Some((earlier_last, earlier)) = head.split_last() else {
        return last.clone();
    };
    if earlier.is_empty() {
        return format!("{earlier_last} and {last}");
    }
    format!("{} and {last}", head.join(", "))
}

/// The card: the question, what ends, how to answer, and the two buttons.
fn card(
    colors: &DialogColors,
    question: &CloseQuestion,
    context: &mut Context<'_, WindowShell>,
) -> AnyElement {
    v_flex()
        .id("close-confirmation-card")
        .test_support()
        .role(Role::Dialog)
        .aria_label("Close confirmation")
        .w_96()
        .gap_3()
        .p_5()
        .rounded_lg()
        .border_1()
        .border_color(colors.border)
        .bg(colors.field)
        .text_color(colors.foreground)
        .shadow_lg()
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, application| {
            application.stop_propagation();
        })
        .on_click(|_, _, application| {
            application.stop_propagation();
        })
        .child(
            div()
                .id("close-confirmation-title")
                .test_support()
                .text_lg()
                .child(question.title.clone()),
        )
        .child(
            div()
                .id("close-confirmation-detail")
                .test_support()
                .text_sm()
                .text_color(colors.quiet)
                .child(question.detail.clone()),
        )
        .child(
            div()
                .id("close-confirmation-hint")
                .test_support()
                .text_xs()
                .text_color(colors.quiet)
                .child(DISMISS_HINT),
        )
        .child(action_row(question, context))
        .into_any_element()
}

/// Cancel, then the close, aligned to the trailing edge.
fn action_row(question: &CloseQuestion, context: &mut Context<'_, WindowShell>) -> AnyElement {
    let confirm = question.confirm.clone();
    h_flex()
        .w_full()
        .justify_end()
        .gap_2()
        .child(
            Button::new("close-confirmation-cancel")
                .label(CANCEL_LABEL)
                .ghost()
                .on_click(context.listener(|shell, _, _, context| {
                    shell.dismiss_close(context);
                })),
        )
        .child(
            Button::new("close-confirmation-confirm")
                .label(confirm)
                .custom(confirm_style(context))
                .on_click(context.listener(|shell, _, window, context| {
                    shell.accept_close(window, context);
                })),
        )
        .into_any_element()
}

/// The close button's colours.
///
/// The kit's danger variant paints its label with the danger fill, so the
/// words sit red on red. This uses the same pair as a tab's close mark: the
/// danger fill, and the foreground the theme gives for text on that fill.
fn confirm_style(context: &gpui_kit::App) -> ButtonCustomVariant {
    let color = context.theme().danger;
    let foreground = context.theme().danger_foreground;
    ButtonCustomVariant::new(context)
        .color(color)
        .foreground(foreground)
        .hover(color)
        .active(color)
}
