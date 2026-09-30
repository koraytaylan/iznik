//! The uploads panel: every file pasted into a pane, and how far each one
//! has been sent.
//!
//! It docks at the right of the panes. A button at the right of the session
//! bar shows and hides it, and so do View → Uploads and its shortcut. It
//! opens when a paste starts, unless it was closed during that paste.
//! Closing it does not stop a file that is already on its way. A directory
//! is one row. The list is the main window's, so opening the panel again
//! shows the same history.

use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{Icon, IconName, Sizable as _, Size, Theme};
use gpui_kit::{
    AnyElement, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, Role,
    SharedString, StatefulInteractiveElement as _, Styled, TestSupportExt, WeakEntity, div, px,
    relative,
};

use crate::upload::{
    UploadGroup, UploadPhase, UploadRecord, busy, byte_pair, failure_line, filled, groups, label,
    panel_line, place_line, status_line,
};
use crate::upload_rate::{RateTrack, line, percent_width};
use crate::window::WindowShell;

/// The panel's width as a share of the pane row, near an editor sidebar,
/// before [`PANEL_MINIMUM`] and [`PANEL_MAXIMUM`] clamp it.
const PANEL_SHARE: f32 = 0.28;
/// Narrowest the panel is drawn. Below this the name and the byte line collide.
const PANEL_MINIMUM: f32 = 260.0;
/// Widest the panel is drawn, so a large window keeps the terminal in front.
const PANEL_MAXIMUM: f32 = 420.0;
/// Space inside a row and the header.
const ROW_PADDING: f32 = 12.0;
/// Space between the lines of a row.
const ROW_GAP: f32 = 2.0;
/// Space between an icon and the text beside it.
const CONTENT_GAP: f32 = 8.0;
/// How large a file or directory icon is.
const ICON_SIZE: f32 = 16.0;
/// How large the finished, failed and hide marks are.
const MARK_SIZE: f32 = 14.0;
/// The session-bar button, square. It sits inside the bar's own height.
const BUTTON_SIZE: f32 = 22.0;
/// The dot shown on that button while a transfer is under way and the panel is closed.
const DOT_SIZE: f32 = 6.0;
/// How far that dot sits in from the button's corner.
const DOT_OFFSET: f32 = 2.0;
/// How large the icon in the empty panel is.
const EMPTY_ICON: f32 = 28.0;
/// Title shown at the top of the panel and on the session-bar button.
const PANEL_TITLE: &str = "Uploads";
/// What the hide button is called.
const HIDE_LABEL: &str = "Hide uploads";
/// Shown when nothing has been pasted.
const EMPTY_TITLE: &str = "Nothing pasted yet";
/// The sentence under [`EMPTY_TITLE`].
const EMPTY_DETAIL: &str = "Paste a file or a directory into the focused pane.";
/// Drops finished and failed pastes from the list.
const CLEAR_LABEL: &str = "Clear";
/// Stops the paste this row is showing.
const CANCEL_LABEL: &str = "Cancel";
/// Drops one finished or failed paste.
const REMOVE_LABEL: &str = "Remove";
/// Room at the right of the list so the scrollbar does not cover a row button.
///
/// The kit draws that bar 16 pixels wide.
const SCROLL_RESERVE: f32 = 16.0;

/// The panel, when it is open.
#[must_use]
pub fn panel(
    shell: &WindowShell,
    theme: &Theme,
    entity: &WeakEntity<WindowShell>,
) -> Option<AnyElement> {
    if shell.pending_upload.panel != crate::upload::UploadPanel::Open {
        return None;
    }
    let found = groups(&shell.pending_upload.records);
    let summary = panel_line(&found);
    let can_clear = found.iter().any(|group| !busy(&group.records));
    let handle = &shell.upload_scroll;
    let mut list = div()
        .id("uploads-list")
        .size_full()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .track_scroll(handle)
        .pr(px(SCROLL_RESERVE));
    if found.is_empty() {
        list = list.child(empty(theme));
    }
    for group in &found {
        list = list.child(entry(group, &shell.pending_upload.rate, theme, entity));
    }
    Some(
        div()
            .id("uploads-panel")
            .test_support()
            .role(Role::Group)
            .aria_label(PANEL_TITLE)
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .overflow_hidden()
            .w(relative(PANEL_SHARE))
            .min_w(px(PANEL_MINIMUM))
            .max_w(px(PANEL_MAXIMUM))
            .flex_shrink(0.0)
            .flex_grow(0.0)
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_l_1()
            .border_color(theme.sidebar_border)
            .child(header(theme, &summary, can_clear, entity))
            .child(
                div()
                    .id("uploads-scroll")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(list)
                    .child(
                        Scrollbar::vertical(handle)
                            .id("uploads-scrollbar")
                            .mode(ScrollbarMode::Always)
                            .styles(|styles| {
                                styles.thumb(|style| style.bg(theme.muted_foreground))
                            }),
                    ),
            )
            .into_any_element(),
    )
}

/// The button at the right of the session bar.
#[must_use]
pub fn session_button(
    theme: &Theme,
    shell: &WindowShell,
    entity: &WeakEntity<WindowShell>,
) -> AnyElement {
    let open = shell.pending_upload.panel == crate::upload::UploadPanel::Open;
    let active = busy(&shell.pending_upload.records);
    let color = if open {
        theme.foreground
    } else if active {
        theme.progress_bar
    } else {
        theme.muted_foreground
    };
    let target = entity.clone();
    let mut button = div()
        .id("uploads-button")
        .test_support()
        .role(Role::Button)
        .aria_label(PANEL_TITLE)
        .aria_expanded(open)
        .relative()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(BUTTON_SIZE))
        .rounded_md()
        .cursor_pointer()
        .text_color(color)
        .hover(|style| style.bg(theme.button_hover).text_color(theme.foreground))
        .child(
            Icon::new(IconName::PanelRight)
                .size(px(ICON_SIZE))
                .text_color(color),
        )
        .on_click(move |_event, _window, application| {
            let _ignored = target.update(application, |window_shell, context| {
                crate::upload::show(window_shell, context);
            });
        });
    if open {
        button = button.bg(theme.muted);
    }
    if active && !open {
        button = button.child(
            div()
                .absolute()
                .top(px(DOT_OFFSET))
                .right(px(DOT_OFFSET))
                .size(px(DOT_SIZE))
                .rounded_full()
                .bg(theme.progress_bar),
        );
    }
    button.into_any_element()
}

/// The panel's title row: the name, how the list stands, and hide.
fn header(
    theme: &Theme,
    summary: &str,
    can_clear: bool,
    entity: &WeakEntity<WindowShell>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h_10()
        .px(px(ROW_PADDING))
        .flex_shrink_0()
        .gap(px(CONTENT_GAP))
        .border_b_1()
        .border_color(theme.sidebar_border)
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(PANEL_TITLE),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(CONTENT_GAP))
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(summary.to_owned()),
                )
                .child(clear_button(theme, can_clear, entity))
                .child(hide_button(theme, entity)),
        )
}

/// Drops finished and failed pastes. Nothing, when every paste is still sending.
fn clear_button(theme: &Theme, can_clear: bool, entity: &WeakEntity<WindowShell>) -> AnyElement {
    if !can_clear {
        return div().into_any_element();
    }
    let target = entity.clone();
    div()
        .id("uploads-clear")
        .test_support()
        .role(Role::Button)
        .aria_label(CLEAR_LABEL)
        .flex_shrink_0()
        .px_2()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|style| style.bg(theme.button_hover).text_color(theme.foreground))
        .child(CLEAR_LABEL)
        .on_click(move |_event, _window, application| {
            let _ignored = target.update(application, |window_shell, context| {
                crate::upload_list::clear_settled(window_shell, context);
            });
        })
        .into_any_element()
}

/// Hides the panel without stopping a transfer.
fn hide_button(theme: &Theme, entity: &WeakEntity<WindowShell>) -> impl IntoElement {
    let target = entity.clone();
    div()
        .id("uploads-hide")
        .test_support()
        .role(Role::Button)
        .aria_label(HIDE_LABEL)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(BUTTON_SIZE))
        .rounded_md()
        .cursor_pointer()
        .hover(|style| style.bg(theme.button_hover))
        .child(
            Icon::new(IconName::Close)
                .size(px(MARK_SIZE))
                .text_color(theme.sidebar_foreground),
        )
        .on_click(move |_event, _window, application| {
            let _hidden = target.update(application, |window_shell, context| {
                crate::upload::show(window_shell, context);
            });
        })
}

/// What the panel says before the first paste.
fn empty(theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(CONTENT_GAP))
        .px(px(ROW_PADDING))
        .child(
            Icon::new(IconName::Folder)
                .size(px(EMPTY_ICON))
                .text_color(theme.muted_foreground),
        )
        .child(div().text_sm().child(EMPTY_TITLE))
        .child(
            div()
                .text_xs()
                .text_center()
                .text_color(theme.muted_foreground)
                .child(EMPTY_DETAIL),
        )
}

/// One paste: a directory as a single row, or one file.
fn entry(
    group: &UploadGroup,
    rate: &[RateTrack],
    theme: &Theme,
    entity: &WeakEntity<WindowShell>,
) -> impl IntoElement {
    let color = icon_color(&group.records, theme);
    let mut lines = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(ROW_GAP))
        .child(title_row(group, theme, color, entity))
        .child(quiet(theme, &status_line(group)))
        .child(quiet(theme, &place_line(group)));
    if busy(&group.records)
        && let Some(text) = rate_line(group, rate)
    {
        lines = lines.child(quiet(theme, &text));
    }
    if group
        .records
        .iter()
        .any(|record| record.directory && record.type_path)
        && let Some(detail) = failure_line(group)
    {
        lines = lines.child(
            div()
                .text_xs()
                .text_color(theme.danger)
                .text_ellipsis()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(detail.to_owned()),
        );
    }
    div()
        .flex()
        .items_start()
        .gap(px(CONTENT_GAP))
        .px(px(ROW_PADDING))
        .py(px(ROW_PADDING))
        .border_b_1()
        .border_color(theme.sidebar_border)
        .flex_shrink_0()
        .child(
            Icon::new(entry_icon(group))
                .size(px(ICON_SIZE))
                .text_color(color)
                .flex_shrink_0(),
        )
        .child(lines.child(progress(group, theme)))
}

/// Cancel, while the paste is moving, or remove, once it has settled.
fn row_action(group: &UploadGroup, theme: &Theme, entity: &WeakEntity<WindowShell>) -> AnyElement {
    let Some(record) = group
        .records
        .iter()
        .find(|record| record.type_path)
        .or(group.records.first())
    else {
        return div().into_any_element();
    };
    let host = record.host.clone();
    let pane = record.pane;
    let name = record.name.clone();
    let moving = busy(&group.records);
    let label = if moving { CANCEL_LABEL } else { REMOVE_LABEL };
    let target = entity.clone();
    div()
        .id(SharedString::from(format!(
            "upload-action-{}-{}-{name}",
            host, pane.0
        )))
        .test_support()
        .role(Role::Button)
        .aria_label(label)
        .flex_shrink_0()
        .px_2()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .text_color(if moving {
            theme.danger
        } else {
            theme.muted_foreground
        })
        .hover(|style| style.bg(theme.button_hover).text_color(theme.foreground))
        .child(label)
        .on_click(move |_event, _window, application| {
            let host = host.clone();
            let name = name.clone();
            let _ignored = target.update(application, |window_shell, context| {
                if moving {
                    crate::upload_list::cancel_group(window_shell, &host, pane, &name, context);
                } else {
                    crate::upload_list::remove_group(window_shell, &host, pane, &name, context);
                }
            });
        })
        .into_any_element()
}

/// The name, and how far the paste has gone at the right.
fn title_row(
    group: &UploadGroup,
    theme: &Theme,
    color: Hsla,
    entity: &WeakEntity<WindowShell>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(CONTENT_GAP))
        .min_w_0()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label(group).to_owned()),
        )
        .child(mark(group, theme, color))
        .child(row_action(group, theme, entity))
}

/// A secondary line.
fn quiet(theme: &Theme, text: &str) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(text.to_owned())
}

/// A check when the paste is done, a cross when it failed, otherwise the percent.
fn mark(group: &UploadGroup, theme: &Theme, color: Hsla) -> AnyElement {
    let records = &group.records;
    if records
        .iter()
        .all(|record| record.phase == UploadPhase::Finished)
    {
        return Icon::new(IconName::CircleCheck)
            .size(px(MARK_SIZE))
            .text_color(theme.success)
            .into_any_element();
    }
    if !busy(records)
        && records
            .iter()
            .any(|record| record.phase == UploadPhase::Failed)
    {
        return Icon::new(IconName::CircleX)
            .size(px(MARK_SIZE))
            .text_color(theme.danger)
            .into_any_element();
    }
    div()
        .flex_shrink_0()
        .text_xs()
        .text_color(color)
        .whitespace_nowrap()
        .child(percent_mark(records))
        .into_any_element()
}

/// The bar under the text. Its fill is the theme's progress colour, by the bytes sent.
fn progress(group: &UploadGroup, theme: &Theme) -> impl IntoElement {
    let (sent, total) = byte_pair(&group.records);
    let failed = !busy(&group.records)
        && group
            .records
            .iter()
            .any(|record| record.phase == UploadPhase::Failed);
    let color = if failed {
        theme.danger
    } else {
        theme.progress_bar
    };
    Progress::new(SharedString::from(bar_name(group)))
        .with_size(Size::XSmall)
        .value(percent_width(sent, total))
        .color(color)
}

/// A stable name for one paste's bar, so its width animates in place.
fn bar_name(group: &UploadGroup) -> String {
    let Some(record) = group
        .records
        .iter()
        .find(|record| record.type_path)
        .or(group.records.first())
    else {
        return "upload".to_owned();
    };
    format!("{}:{}:{}", record.host, record.pane.0, record.name)
}

/// Bytes per second and the time still left, while a paste is moving.
fn rate_line(group: &UploadGroup, rate: &[RateTrack]) -> Option<String> {
    let record = group
        .records
        .iter()
        .find(|record| record.type_path)
        .or(group.records.first())?;
    let (_, total) = byte_pair(&group.records);
    line(rate, &record.host, record.pane, &record.name, total)
}

/// The percent beside the name. A transfer under one percent still says so.
fn percent_mark(records: &[UploadRecord]) -> String {
    let whole = filled(records);
    let (sent, total) = byte_pair(records);
    if whole == 0 && sent > 0 && total > 0 {
        return "< 1%".to_owned();
    }
    format!("{whole}%")
}

/// A directory the person pasted uses a folder icon. Anything else is a file.
fn entry_icon(group: &UploadGroup) -> IconName {
    if group
        .records
        .iter()
        .any(|record| record.directory && record.type_path)
    {
        IconName::Folder
    } else {
        IconName::File
    }
}

/// The theme's progress colour while bytes are moving, danger when the host refused.
fn icon_color(records: &[UploadRecord], theme: &Theme) -> Hsla {
    if !busy(records)
        && records
            .iter()
            .any(|record| record.phase == UploadPhase::Failed)
    {
        theme.danger
    } else if busy(records) {
        theme.progress_bar
    } else {
        theme.sidebar_foreground
    }
}
