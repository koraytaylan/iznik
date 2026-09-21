//! What the window's body shows while no tab is visible: the one next step a
//! person has, for the host they are working with.
//!
//! With nothing held, that is choosing a host — the concrete aliases the ssh
//! configuration already names, one click each, and a button for one it does
//! not name yet. While a host is being reached it is watching that happen,
//! with a way to stop it. When it could not be reached it is the reason, and
//! retrying or removing it. When it is reached and holds no session it is
//! starting one. There is never a blank body that leaves a person guessing
//! which command comes next.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Sizable, Theme, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, TestSupportExt, div,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;

use crate::actions::{ActionId, INVENTORY};
use crate::host_ui::EngineState;
use crate::status::{self, Summary};
use crate::window::WindowShell;

/// The one thing the body offers while no tab is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// No host is held: choose one, from the ssh configuration's own aliases
    /// or by adding one it does not name.
    Welcome {
        /// The concrete aliases the ssh configuration defines, in the order it
        /// defines them, offered as one-click choices.
        configured: Vec<String>,
    },
    /// A host is held and is not connected: watch it, retry it or remove it.
    Host {
        /// The host being described.
        host: HostId,
        /// How its state reads.
        summary: Summary,
    },
    /// A connected host holds no session: start one.
    Empty {
        /// The connected host.
        host: HostId,
    },
}

/// The stage for the current state, describing the preferred host when it is
/// still held and the first held host otherwise.
///
/// `configured` is the ssh configuration's own aliases: with nothing held they
/// are what the welcome offers, and they are read from the shell so the body
/// never has to reach for the file itself.
#[must_use]
pub fn stage(state: &EngineState, preferred: Option<&HostId>, configured: &[String]) -> Stage {
    let chosen = preferred
        .and_then(|host| state.host(host).map(|report| (host, report)))
        .or_else(|| state.hosts().next());
    let Some((host, report)) = chosen else {
        return Stage::Welcome {
            configured: configured.to_vec(),
        };
    };
    if matches!(report.connection, HostState::Connected { .. }) {
        Stage::Empty { host: host.clone() }
    } else {
        Stage::Host {
            host: host.clone(),
            summary: status::summary(host, &report.connection),
        }
    }
}

/// The host a stage describes, so the strips above it do not repeat it.
#[must_use]
pub fn host(stage: &Stage) -> Option<&HostId> {
    match stage {
        Stage::Welcome { .. } => None,
        Stage::Host { host, .. } | Stage::Empty { host } => Some(host),
    }
}

/// The default chord of an inventory action, for a hint beside its button.
fn chord(action: ActionId) -> Option<&'static str> {
    INVENTORY
        .iter()
        .find(|specification| specification.id == action)
        .and_then(|specification| specification.keybinding)
}

/// A primary button that runs an inventory action through the palette path.
fn action_button(
    identifier: &'static str,
    label: &'static str,
    action: ActionId,
    context: &mut Context<'_, WindowShell>,
) -> AnyElement {
    let hint = chord(action)
        .map(|keys| format!("  {keys}"))
        .unwrap_or_default();
    Button::new(identifier)
        .label(format!("{label}{hint}"))
        .primary()
        .large()
        .on_click(context.listener(move |shell, _, window, context| {
            shell.palette_mut().open();
            shell.choose(Some(action), window, context);
            if shell.palette().prompt.is_none() {
                shell.palette_mut().close();
            }
        }))
        .into_any_element()
}

/// The centred card every stage is drawn in.
fn card(
    theme: &Theme,
    identifier: &'static str,
    title: String,
    body: Vec<AnyElement>,
) -> AnyElement {
    div()
        .id("stage")
        .test_support()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            v_flex()
                .id(identifier)
                .test_support()
                .max_w_128()
                .gap_3()
                .p_6()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .child(
                    div()
                        .text_lg()
                        .text_color(theme.popover_foreground)
                        .child(title),
                )
                .children(body),
        )
        .into_any_element()
}

/// A paragraph of explanation in the muted colour.
fn paragraph(theme: &Theme, text: String) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(text)
        .into_any_element()
}

/// One host the ssh configuration names, as a button that adds and connects
/// it at once.
fn configured_button(alias: &str, context: &mut Context<'_, WindowShell>) -> AnyElement {
    let target = alias.to_owned();
    let about = HostId(alias.to_owned());
    Button::new(gpui_kit::SharedString::from(format!("stage-host-{alias}")))
        .label(alias.to_owned())
        .outline()
        .large()
        .on_click(context.listener(move |shell, _, _, context| {
            if let Err(error) = shell.add_host(&target) {
                shell.failure(&about, error.to_string(), context);
            }
        }))
        .into_any_element()
}

/// The hosts the ssh configuration names, as a bounded list of one-click
/// choices; no configuration names none.
fn configured_list(
    configured: &[String],
    context: &mut Context<'_, WindowShell>,
) -> Option<AnyElement> {
    if configured.is_empty() {
        return None;
    }
    Some(
        v_flex()
            .id("stage-configured")
            .gap_2()
            .max_h_128()
            .overflow_y_scroll()
            .test_support()
            .children(
                configured
                    .iter()
                    .map(|alias| configured_button(alias, context)),
            )
            .into_any_element(),
    )
}

/// Draw a stage.
pub fn render(theme: &Theme, stage: &Stage, context: &mut Context<'_, WindowShell>) -> AnyElement {
    let palette_hint = paragraph(
        theme,
        "Every command is in the palette: ctrl-shift-p.".to_owned(),
    );
    match stage {
        Stage::Welcome { configured } => {
            let mut body = vec![paragraph(
                theme,
                "iznik keeps your terminals running on a host, so they survive a closed \
                 window or a dropped network. Choose a host your ssh configuration names, \
                 add one it does not, or connect a local server by unix:/path/to/socket."
                    .to_owned(),
            )];
            // The button is "another" only when the list above it already
            // offered one, so the words match what is on the screen.
            let label = if configured.is_empty() {
                "Add a host"
            } else {
                "Add another host"
            };
            if let Some(list) = configured_list(configured, context) {
                body.push(list);
            }
            body.push(
                h_flex()
                    .child(action_button(
                        "stage-add-host",
                        label,
                        ActionId::AddHost,
                        context,
                    ))
                    .into_any_element(),
            );
            body.push(palette_hint);
            card(theme, "stage-welcome", "Connect to a host".to_owned(), body)
        }
        Stage::Host { host, summary } => {
            let mut body = Vec::new();
            if let Some(detail) = &summary.detail {
                body.push(
                    div()
                        .id("stage-detail")
                        .test_support()
                        .text_sm()
                        .text_color(theme.popover_foreground)
                        .child(detail.clone())
                        .into_any_element(),
                );
            }
            body.push(
                h_flex()
                    .gap_2()
                    .child(status::dot(theme, summary.tone))
                    .children(status::remedies(host, summary, context))
                    .into_any_element(),
            );
            card(theme, "stage-host", summary.headline.clone(), body)
        }
        Stage::Empty { host } => card(
            theme,
            "stage-empty",
            format!("Connected to {}", host.0),
            vec![
                paragraph(
                    theme,
                    "This host holds no sessions yet. A session keeps its tabs and panes \
                     running on the host until you close it."
                        .to_owned(),
                ),
                h_flex()
                    .child(action_button(
                        "stage-new-session",
                        "Start a session",
                        ActionId::CreateSession,
                        context,
                    ))
                    .into_any_element(),
                palette_hint,
            ],
        ),
    }
}
