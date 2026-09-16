//! The application: one GPUI window over the engine, and the themed empty view the chrome grows from.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod actions;
pub mod bars;
pub mod bridge;
pub mod bundle;
pub mod grid;
pub mod host_ui;
pub mod input;
pub mod layout;
pub mod palette;
pub mod settings;
pub mod splits;
pub mod surface;
pub mod theme;
pub mod vt;
pub mod window;

use gpui_kit::TestSupportExt;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement,
    Styled, Window, div,
};

/// The surface the window shows while nothing is attached to the engine.
///
/// Themed from the kit's active theme, so the background and the foreground
/// are the same colors the components around it will draw with. The surface
/// and its label carry stable element ids and test observations, which is what
/// the headless smoke test asserts against.
#[derive(Debug)]
pub struct EmptyView;

impl Render for EmptyView {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = context.theme();
        div()
            .id("empty-view-surface")
            .test_support()
            .aria_label("iznik")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(div().id("empty-view-label").test_support().child("iznik"))
    }
}
