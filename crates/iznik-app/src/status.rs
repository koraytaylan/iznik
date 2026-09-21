//! How a host's connection reads to a person: one headline, the detail that
//! explains it, the tone it is shown in, and what can be done about it.
//!
//! The stage, the host strips above the panes and the session bar all say the
//! same thing about a host, so they all read it from here.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Sizable, Theme, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;

use crate::host_ui::EngineState;
use crate::window::WindowShell;

/// The most lines of a host's error a strip shows before clipping it.
const DETAIL_LINES: usize = 2;

/// The colour family a host's state is shown in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Nothing is happening and nothing is wrong.
    Quiet,
    /// Work is under way.
    Progress,
    /// Connected.
    Good,
    /// The link went and is being tried again.
    Warning,
    /// The host could not be reached.
    Danger,
}

/// Something a person can do about a host from where its state is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// Try the connection again now, without waiting out the backoff.
    Retry,
    /// Stop holding the host.
    Remove,
    /// Stop a connection attempt that is still under way.
    Cancel,
}

impl Remedy {
    /// The button label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Remedy::Retry => "Retry now",
            Remedy::Remove => "Remove host",
            Remedy::Cancel => "Cancel",
        }
    }
}

/// A host's state as a person reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The colour family.
    pub tone: Tone,
    /// One short sentence naming the host.
    pub headline: String,
    /// What explains it, in the words whatever said it used.
    pub detail: Option<String>,
    /// What can be done about it, most useful first.
    pub remedies: Vec<Remedy>,
}

/// Read one host's state.
#[must_use]
pub fn summary(host: &HostId, state: &HostState) -> Summary {
    let alias = &host.0;
    let (tone, headline, detail, remedies) = match state {
        HostState::Disconnected => (
            Tone::Quiet,
            format!("Disconnected from {alias}"),
            None,
            vec![Remedy::Retry, Remedy::Remove],
        ),
        HostState::Probing => (
            Tone::Progress,
            format!("Connecting to {alias}\u{2026}"),
            Some("Checking what the host is running".to_owned()),
            vec![Remedy::Cancel],
        ),
        HostState::Bootstrapping { stage } => (
            Tone::Progress,
            format!("Setting up {alias}\u{2026}"),
            Some(stage.to_string()),
            vec![Remedy::Cancel],
        ),
        HostState::Connecting => (
            Tone::Progress,
            format!("Connecting to {alias}\u{2026}"),
            Some("Starting iznik-server".to_owned()),
            vec![Remedy::Cancel],
        ),
        HostState::Upgrading => (
            Tone::Warning,
            format!("Upgrading {alias}\u{2019}s server\u{2026}"),
            Some("Replacing iznik-server; the sessions it held end".to_owned()),
            Vec::new(),
        ),
        HostState::Connected { server_version, .. } => (
            Tone::Good,
            format!("Connected to {alias}"),
            Some(format!("iznik-server {server_version}")),
            Vec::new(),
        ),
        HostState::Reconnecting {
            attempt, trouble, ..
        } => (
            Tone::Warning,
            format!("Reconnecting to {alias}\u{2026}"),
            Some(trouble.as_ref().map_or_else(
                || format!("Attempt {attempt}"),
                |said| format!("Attempt {attempt}: {said}"),
            )),
            vec![Remedy::Retry, Remedy::Remove],
        ),
        HostState::Failed { error, .. } => (
            Tone::Danger,
            format!("Couldn\u{2019}t connect to {alias}"),
            Some(error.clone()),
            vec![Remedy::Retry, Remedy::Remove],
        ),
    };
    Summary {
        tone,
        headline,
        detail,
        remedies,
    }
}

/// One word for a host's state, for places with room for no more.
#[must_use]
pub fn word(state: &HostState) -> &'static str {
    match state {
        HostState::Disconnected => "disconnected",
        HostState::Probing | HostState::Connecting => "connecting",
        HostState::Bootstrapping { .. } => "setting up",
        HostState::Upgrading => "upgrading",
        HostState::Connected { .. } => "connected",
        HostState::Reconnecting { .. } => "reconnecting",
        HostState::Failed { .. } => "unreachable",
    }
}

/// The theme colour of a tone.
#[must_use]
pub fn color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Quiet => theme.muted_foreground,
        Tone::Progress => theme.info,
        Tone::Good => theme.success,
        Tone::Warning => theme.warning,
        Tone::Danger => theme.danger,
    }
}

/// A small filled circle in a tone's colour.
#[must_use]
pub fn dot(theme: &Theme, tone: Tone) -> AnyElement {
    div()
        .size_2()
        .flex_shrink_0()
        .rounded_full()
        .bg(color(theme, tone))
        .into_any_element()
}

/// The hosts that need a strip above the panes: every held host that is not
/// connected, except the one the stage is already describing.
#[must_use]
pub fn troubled<'state>(
    state: &'state EngineState,
    staged: Option<&HostId>,
) -> Vec<(&'state HostId, Summary)> {
    state
        .hosts()
        .filter(|(host, report)| {
            !matches!(report.connection, HostState::Connected { .. }) && Some(*host) != staged
        })
        .map(|(host, report)| (host, summary(host, &report.connection)))
        .collect()
}

/// The buttons for a summary's remedies, each wired to the shell.
pub fn remedies(
    host: &HostId,
    summary: &Summary,
    context: &mut Context<'_, WindowShell>,
) -> Vec<AnyElement> {
    summary
        .remedies
        .iter()
        .enumerate()
        .map(|(index, remedy)| {
            let target = host.clone();
            let remedy = *remedy;
            let button = Button::new(SharedString::from(format!(
                "{}-{}",
                remedy.label().to_lowercase().replace(' ', "-"),
                host.0
            )))
            .label(remedy.label())
            .small()
            .on_click(context.listener(move |shell, _, _, context| {
                shell.remedy(&target, remedy, context);
            }));
            if index == 0 {
                button.primary().into_any_element()
            } else {
                button.ghost().into_any_element()
            }
        })
        .collect()
}

/// One readable strip for a host that is not connected, with its remedies.
pub fn banner(
    theme: &Theme,
    host: &HostId,
    summary: &Summary,
    context: &mut Context<'_, WindowShell>,
) -> AnyElement {
    let label = format!("{}: {}", host.0, summary.headline);
    let mut text = div().flex().flex_col().flex_1().min_w_0().child(
        div()
            .text_sm()
            .text_color(theme.foreground)
            .child(summary.headline.clone()),
    );
    if let Some(detail) = &summary.detail {
        text = text.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .line_clamp(DETAIL_LINES)
                .child(detail.clone()),
        );
    }
    h_flex()
        .id(SharedString::from(format!("host-banner-{}", host.0)))
        .test_support()
        .aria_label(label)
        .w_full()
        .flex_shrink_0()
        .gap_3()
        .px_3()
        .py_2()
        .bg(theme.secondary)
        .border_b_1()
        .border_color(theme.border)
        .child(dot(theme, summary.tone))
        .child(text)
        .children(remedies(host, summary, context))
        .into_any_element()
}

/// Raise one toast per host whose connected server is offering an upgrade,
/// saying why and that upgrading the host — which ends its sessions — is what
/// fixes it.
///
/// Keyed off the offer, not a guess: the offer is the one thing that already
/// knows whether the difference is a version or a missing feature, and it is
/// what the palette's `host: upgrade` acts on, so the toast and the action can
/// never disagree. A host of another version is offered an upgrade on the
/// version alone, and its capabilities are not believed; a host of this
/// version missing a feature is offered one for the gap. Either way the person
/// is told, because otherwise the only sign is a disabled menu entry.
///
/// Once per host while it stays on offer: the engine says a host's state on
/// every event, and a toast on each would be a toast a person cannot read. A
/// host that has been upgraded, or removed, is forgotten, so a host that comes
/// back on offer is told about again. The kit's own `push_notification` panics
/// unless the window's root view is its `Root`, and a headless fixture that
/// puts the shell directly at the root is a legitimate window, so the toast is
/// shown when there is somewhere to put it and skipped when there is not.
pub fn notify_upgrades_on_offer(
    shell: &mut WindowShell,
    window: &mut Window,
    context: &mut Context<'_, WindowShell>,
) {
    let offering: Vec<(HostId, String)> = shell
        .hosts()
        .state()
        .hosts()
        .filter(|(host, _report)| !shell.upgrade_notices.contains(*host))
        .filter_map(|(host, report)| {
            let offer = report.upgrade.as_ref()?;
            Some((
                host.clone(),
                format!(
                    "{}. This ends every session on the host; `host: upgrade` in the command \
                     palette, or the host's menu, is how to do it.",
                    offer.summary(host)
                ),
            ))
        })
        .collect();
    for (host, message) in offering {
        let has_root = window
            .root::<gpui_kit::component::Root>()
            .flatten()
            .is_some();
        if has_root {
            window.push_notification(Notification::warning(message), context);
        }
        let _noted = shell.upgrade_notices.insert(host);
    }
    // A host no longer on offer — upgraded, or gone — is forgotten, so a host
    // that comes back on offer is told about again.
    let still_offering: Vec<HostId> = shell
        .upgrade_notices
        .iter()
        .filter(|host| {
            shell
                .hosts()
                .state()
                .host(host)
                .is_some_and(|report| report.upgrade.is_some())
        })
        .cloned()
        .collect();
    shell.upgrade_notices = still_offering.into_iter().collect();
}
