//! The window's body and banners: the visible tab's pane grid, the stage that
//! describes the next step while no tab is visible, and the strips that say
//! what is wrong.
//!
//! Rendering only: what to show is read from the shell's settled state, and
//! the one thing that writes back — a pane's measured size — goes through the
//! shell's own `measured`, which submits against the authoritative geometry.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::{ActiveTheme as _, ElementExt as _};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, TestSupportExt,
    WeakEntity, div, px,
};

use crate::grid;
use crate::host_ui::Notice;
use crate::stage;
use crate::status;
use crate::vt::PaneKey;
use crate::window::{TERMINAL_PADDING, WindowShell};

impl WindowShell {
    /// The body under the bars: the visible tab's pane grid, or the stage
    /// describing the next step while no tab is visible.
    pub(crate) fn body(
        &self,
        entity: &WeakEntity<WindowShell>,
        context: &mut Context<'_, Self>,
    ) -> AnyElement {
        if let (Some(selected), Some(layout)) = (&self.selected, &self.layout) {
            crate::splits::render_interactive(
                layout,
                self.revision,
                |pane| {
                    let key = PaneKey {
                        host: selected.host.clone(),
                        pane,
                    };
                    let Some(held) = self.panes.get(&key) else {
                        return div().into_any_element();
                    };
                    let entity = entity.clone();
                    // The padding is outside the measured box, so the columns
                    // and rows sent to the host are the ones that fit inside it.
                    div()
                        .size_full()
                        .overflow_hidden()
                        .p(px(TERMINAL_PADDING))
                        .child(
                            div()
                                .size_full()
                                .overflow_hidden()
                                .child(held.surface.clone())
                                .on_prepaint(move |bounds, window, application| {
                                    let entity = entity.clone();
                                    let key = key.clone();
                                    window.defer(application, move |_, application| {
                                        let _updated =
                                            entity.update(application, |shell, update_context| {
                                                shell.measured(&key, bounds.size, update_context);
                                            });
                                    });
                                }),
                        )
                        .into_any_element()
                },
                crate::splits::resize_callback(selected, layout, entity),
            )
        } else {
            let aliases = self.ssh_alias();
            let stage = stage::stage(
                self.hosts().state(),
                self.following.preferred.as_ref(),
                &aliases,
            );
            let theme = context.theme().clone();
            stage::render(&theme, &stage, context)
        }
    }

    /// A readable strip for every held host that is not connected and that
    /// the stage is not already describing, then the latest local failure.
    pub(crate) fn banners(&self, context: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let staged = self.selected.is_none().then(|| {
            stage::stage(
                self.hosts().state(),
                self.following.preferred.as_ref(),
                &self.ssh_alias(),
            )
        });
        let troubled: Vec<_> =
            status::troubled(self.hosts().state(), staged.as_ref().and_then(stage::host))
                .into_iter()
                .map(|(host, summary)| (host.clone(), summary))
                .collect();
        let mut banners = Vec::new();
        if !troubled.is_empty() {
            let theme = context.theme().clone();
            for (host, summary) in &troubled {
                banners.push(status::banner(&theme, host, summary, context));
            }
        }
        if let Some(failure) = &self.last_failure {
            banners.push(banner(context, failure));
        }
        banners
    }
}

/// One strip for the latest local failure, dismissible without discarding
/// host state.
///
/// The text colour is set here rather than left to the alert's variant: the
/// kit paints an error alert's message in `theme.danger`, which is the
/// `danger.background` token — a colour meant to sit *behind* text. In a theme
/// that declares it as a dark red, as Ayu Mirage does, that is dark red text on
/// a dark red field, which is a contrast ratio of 1.1 and cannot be read at
/// all. The failure reads in `danger.foreground`, which is the token for text
/// on that colour, so it is legible in every theme.
fn banner(context: &mut Context<'_, WindowShell>, failure: &Notice) -> AnyElement {
    let foreground = context.theme().danger_foreground;
    div()
        .id("surface-failure")
        .test_support()
        .child(
            Alert::error(
                "surface-failure-alert",
                format!("{}: {}", failure.host, failure.detail),
            )
            .banner()
            .text_color(foreground)
            .on_close(context.listener(|shell, _, _, context| {
                shell.last_failure = None;
                context.notify();
            })),
        )
        .into_any_element()
}

/// The terminal color the pane area is painted with, so the grid's own
/// background and the space around it cannot disagree.
#[must_use]
pub fn painted_pane_color(theme: &crate::vt::TerminalTheme) -> gpui_kit::Hsla {
    grid::terminal_color(theme.background)
}
