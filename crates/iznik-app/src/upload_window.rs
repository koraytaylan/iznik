//! The uploads window: every file pasted into a pane, and how far each one
//! has been sent.
//!
//! It opens when a paste starts, and from View → Uploads. Closing it does
//! not stop a file that is already on its way. The list is the main window's,
//! so opening the window again shows the same history.

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme as _, Root, TitleBar};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Hsla, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Subscription, Window, WindowBounds, WindowOptions, div, px, relative, size,
};

use crate::upload::{FULL_PERCENT, UploadPhase, UploadRecord, byte_text, percent};
use crate::window::WindowShell;

/// Initial width of the uploads window.
const WINDOW_WIDTH: f32 = 480.0;
/// Initial height of the uploads window.
const WINDOW_HEIGHT: f32 = 560.0;
/// How thick the progress bar is.
const BAR_HEIGHT: f32 = 6.0;
/// Space around the list.
const PADDING: f32 = 16.0;
/// Space between rows.
const GAP: f32 = 12.0;
/// Space inside a row.
const ROW_GAP: f32 = 4.0;
/// Title shown in the window's title bar.
const WINDOW_TITLE: &str = "Uploads";
/// Shown when nothing has been pasted.
const EMPTY: &str = "No file has been pasted.";

/// Open the uploads window over `context`'s shell.
pub fn open(context: &mut Context<'_, WindowShell>) {
    let shell = context.entity();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            context,
        )),
        ..TitleBar::window_options()
    };
    context.defer(move |app| {
        let _window = app.open_window(options, move |window, app| {
            let view = app.new(|view_context| UploadWindow::new(&shell, view_context));
            app.new(|root_context| Root::new(view, window, root_context))
        });
    });
}

/// The uploads window.
struct UploadWindow {
    /// The main window whose paste history is drawn.
    shell: gpui_kit::WeakEntity<WindowShell>,
    /// Redraws this window when that history changes.
    _watch: Subscription,
}

impl UploadWindow {
    /// Watch `shell`.
    fn new(shell: &Entity<WindowShell>, context: &mut Context<'_, Self>) -> Self {
        let watch = context.observe(shell, |_window, _shell, context| context.notify());
        Self {
            shell: shell.downgrade(),
            _watch: watch,
        }
    }

    /// The history, newest first, or nothing when the main window has closed.
    fn records(&self, app: &App) -> Vec<UploadRecord> {
        self.shell.upgrade().map_or_else(Vec::new, |shell| {
            let mut records = shell.read(app).pending_upload.records.clone();
            records.reverse();
            records
        })
    }
}

impl Render for UploadWindow {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = context.theme().clone();
        let records = self.records(context);
        let mut list = div()
            .id("uploads")
            .flex()
            .flex_col()
            .gap(px(GAP))
            .size_full()
            .p(px(PADDING));
        list = list.child(div().child(WINDOW_TITLE));
        if records.is_empty() {
            list = list.child(div().text_sm().child(EMPTY));
        }
        for record in records {
            list = list.child(row(&record, theme.accent, theme.border));
        }
        list.overflow_y_scrollbar()
    }
}

/// One file: its name, how far it has been sent, and where it landed.
fn row(record: &UploadRecord, accent: Hsla, track: Hsla) -> impl IntoElement {
    let filled = percent(record.sent, record.total);
    let width = relative(f32::from(filled) / f32::from(FULL_PERCENT));
    div()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
        .text_sm()
        .child(div().child(record.name.clone()))
        .child(div().child(status(record)))
        .child(
            div()
                .h(px(BAR_HEIGHT))
                .w(relative(1.0))
                .bg(track)
                .child(div().h(px(BAR_HEIGHT)).w(width).bg(accent)),
        )
}

/// The line under a file's name.
fn status(record: &UploadRecord) -> String {
    let reading = format!("{} / {}", byte_text(record.sent), byte_text(record.total));
    match record.phase {
        UploadPhase::Waiting => format!("waiting · {reading}"),
        UploadPhase::Sending => format!(
            "sending · {reading} · {}%",
            percent(record.sent, record.total)
        ),
        UploadPhase::Finished => record.remote.clone().map_or_else(
            || format!("finished · {reading}"),
            |path| format!("finished · {path}"),
        ),
        UploadPhase::Failed => record.detail.clone().map_or_else(
            || format!("failed · {reading}"),
            |detail| format!("failed · {detail}"),
        ),
    }
}
