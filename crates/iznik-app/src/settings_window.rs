//! The settings window: a second OS window listing application settings.

use gpui_kit::component::setting::Settings as SettingsPanel;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::component::{Root, Theme, ThemeRegistry, TitleBar};
use gpui_kit::{
    App, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, TestSupportExt, WeakEntity, Window, WindowBounds, WindowOptions, div, px,
    size,
};

use crate::actions::INVENTORY;
use crate::settings::{Behavior, Settings};
use crate::theme::AppTheme;
use crate::window::WindowShell;

/// Lower bound a font size is clamped to.
const MINIMUM_FONT_SIZE: f32 = 8.0;
/// Upper bound a font size is clamped to.
const MAXIMUM_FONT_SIZE: f32 = 32.0;
/// Tightest row height offered, as a multiple of the font size.
const MINIMUM_LINE_HEIGHT: f32 = 1.0;
/// Loosest row height offered.
const MAXIMUM_LINE_HEIGHT: f32 = 2.0;
/// How far one step of the font size field moves it.
const FONT_SIZE_STEP: f32 = 1.0;
/// How far one step of the row height field moves it.
const LINE_HEIGHT_STEP: f32 = 0.1;
/// Steps per unit of row height, so the field shows `1.2` rather than the
/// `1.2000000476837158` a 32-bit value widens to.
const LINE_HEIGHT_STEPS_PER_UNIT: f64 = 10.0;

/// Initial width of the settings window.
const WINDOW_WIDTH: f32 = 900.0;
/// Initial height of the settings window.
const WINDOW_HEIGHT: f32 = 600.0;
/// Title shown in the settings window's titlebar.
const WINDOW_TITLE: &str = "iznik settings";
/// Shown for a keybinding with no chord in either the settings override or the inventory.
const NO_CHORD: &str = "\u{2014}";

/// Open the settings window over the shell's current, live settings.
pub fn open(context: &mut Context<'_, WindowShell>) {
    let shell = context.entity().downgrade();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            context,
        )),
        ..TitleBar::window_options()
    };
    context.defer(move |app| {
        let _window = app.open_window(options, move |window, app| {
            let view = app.new(|_| SettingsWindow {
                shell: shell.clone(),
            });
            app.new(|root_context| Root::new(view, window, root_context))
        });
    });
}

/// The settings window's root view: a panel over the main window's live settings.
struct SettingsWindow {
    /// The main window this settings window edits.
    shell: WeakEntity<WindowShell>,
}

impl Render for SettingsWindow {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .id("settings-window-content")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(TitleBar::new().child(WINDOW_TITLE))
            .child(
                div().flex_1().min_h_0().child(
                    SettingsPanel::new("iznik-settings")
                        .page(appearance_page(&self.shell, context))
                        .page(terminal_page(&self.shell))
                        .page(keybindings_page(&self.shell)),
                ),
            )
    }
}

/// Read the shell's current theme, or the default when the main window closed.
fn theme_of(shell: &WeakEntity<WindowShell>, app: &App) -> AppTheme {
    shell.upgrade().map_or_else(AppTheme::default, |entity| {
        entity.read(app).settings().theme.clone()
    })
}

/// Apply a theme change to the live shell, when it still exists.
fn edit_theme(shell: &WeakEntity<WindowShell>, app: &mut App, edit: impl FnOnce(&mut AppTheme)) {
    let Some(shell) = shell.upgrade() else {
        return;
    };
    shell.update(app, |shell, context| {
        let mut theme = shell.settings().theme.clone();
        edit(&mut theme);
        shell.set_theme(theme, context);
    });
}

/// The Appearance settings page: theme selection and terminal typography.
fn appearance_page(shell: &WeakEntity<WindowShell>, current: &App) -> SettingPage {
    SettingPage::new("Appearance").group(
        SettingGroup::new()
            .title("Appearance")
            .item(SettingItem::new(
                "Theme",
                SettingField::scrollable_dropdown(theme_options(current), active_theme_name, {
                    let shell = shell.clone();
                    move |name, app| select_theme(&shell, &name, app)
                }),
            ))
            .item(SettingItem::new(
                "Font Family",
                SettingField::scrollable_dropdown(
                    font_options(current),
                    {
                        let shell = shell.clone();
                        move |app| SharedString::from(theme_of(&shell, app).font_family)
                    },
                    {
                        let shell = shell.clone();
                        move |value, app| {
                            edit_theme(&shell, app, |theme| theme.font_family = value.to_string());
                        }
                    },
                ),
            ))
            .item(SettingItem::new(
                "Font Size",
                SettingField::number_input(
                    NumberFieldOptions {
                        min: f64::from(MINIMUM_FONT_SIZE),
                        max: f64::from(MAXIMUM_FONT_SIZE),
                        step: f64::from(FONT_SIZE_STEP),
                    },
                    {
                        let shell = shell.clone();
                        move |app| f64::from(theme_of(&shell, app).font_size)
                    },
                    {
                        let shell = shell.clone();
                        move |value, app| {
                            let font_size = nearest_step(
                                value,
                                MINIMUM_FONT_SIZE,
                                MAXIMUM_FONT_SIZE,
                                FONT_SIZE_STEP,
                            );
                            edit_theme(&shell, app, |theme| theme.font_size = font_size);
                        }
                    },
                ),
            ))
            .item(line_height_item(shell))
            .item(tabs_in_title_bar_item(shell)),
    )
}

/// The field's step nearest to `value`, as the 32-bit value a theme holds.
///
/// A number field hands over a 64-bit value and the theme holds a 32-bit
/// one, and there is no conversion between the two that cannot lose
/// something. So the value is not converted at all: it is clamped to the
/// field's range, and the answer is the step of that range — built in 32 bits
/// from `minimum` and `step` — whose lossless widening lies nearest to it. A
/// value no number is, which a field should never hand over, is `minimum`.
#[must_use]
pub fn nearest_step(value: f64, minimum: f32, maximum: f32, step: f32) -> f32 {
    if value.is_nan() || step.is_nan() || step <= 0.0 || minimum > maximum {
        return minimum;
    }
    let wanted = value.clamp(f64::from(minimum), f64::from(maximum));
    let distance = |candidate: f32| (f64::from(candidate) - wanted).abs();
    let mut nearest = minimum;
    let mut index: u16 = 0;
    loop {
        let candidate = step.mul_add(f32::from(index), minimum).min(maximum);
        if distance(candidate) < distance(nearest) {
            nearest = candidate;
        }
        let Some(next) = index.checked_add(1).filter(|_| candidate < maximum) else {
            return nearest;
        };
        index = next;
    }
}

/// The row height setting: a multiple of the font size.
fn line_height_item(shell: &WeakEntity<WindowShell>) -> SettingItem {
    SettingItem::new(
        "Line Height",
        SettingField::number_input(
            NumberFieldOptions {
                min: f64::from(MINIMUM_LINE_HEIGHT),
                max: f64::from(MAXIMUM_LINE_HEIGHT),
                step: f64::from(LINE_HEIGHT_STEP),
            },
            {
                let shell = shell.clone();
                move |app| {
                    (f64::from(theme_of(&shell, app).line_height) * LINE_HEIGHT_STEPS_PER_UNIT)
                        .round()
                        / LINE_HEIGHT_STEPS_PER_UNIT
                }
            },
            {
                let shell = shell.clone();
                move |value, app| {
                    let line_height = nearest_step(
                        value,
                        MINIMUM_LINE_HEIGHT,
                        MAXIMUM_LINE_HEIGHT,
                        LINE_HEIGHT_STEP,
                    );
                    edit_theme(&shell, app, |theme| theme.line_height = line_height);
                }
            },
        ),
    )
    .description("Row height as a multiple of the font size.")
}

/// The setting that draws the tabs in the window's title bar.
fn tabs_in_title_bar_item(shell: &WeakEntity<WindowShell>) -> SettingItem {
    SettingItem::new(
        "Tabs in Title Bar",
        SettingField::switch(
            {
                let shell = shell.clone();
                move |app| theme_of(&shell, app).tabs_in_title_bar
            },
            {
                let shell = shell.clone();
                move |value, app| edit_theme(&shell, app, |theme| theme.tabs_in_title_bar = value)
            },
        ),
    )
    .description("Show the session's tabs in the window's title bar instead of a bar of their own.")
}

/// The Terminal settings page: what a pane's program may do, and how keys
/// and pastes reach it.
fn terminal_page(shell: &WeakEntity<WindowShell>) -> SettingPage {
    SettingPage::new("Terminal").group(
        SettingGroup::new()
            .title("Terminal")
            .item(
                behavior_item(
                    shell,
                    "Programs May Copy",
                    |behavior| behavior.clipboard_write,
                    |behavior, value| behavior.clipboard_write = value,
                )
                .description(
                    "Let a program in the focused pane put text on the clipboard (OSC 52).",
                ),
            )
            .item(
                behavior_item(
                    shell,
                    "Ask Before Pasting Lines",
                    |behavior| behavior.confirm_multiline_paste,
                    |behavior, value| behavior.confirm_multiline_paste = value,
                )
                .description(
                    "Ask before pasting several lines into a program that would run each one.",
                ),
            )
            .item(
                behavior_item(
                    shell,
                    "Option as Meta",
                    |behavior| behavior.option_as_meta,
                    |behavior, value| behavior.option_as_meta = value,
                )
                .description("Send Option as Meta instead of typing the layout's characters."),
            ),
    )
}

/// One switch over a behavior setting of the live shell.
fn behavior_item(
    shell: &WeakEntity<WindowShell>,
    title: &'static str,
    read: fn(&Behavior) -> bool,
    write: fn(&mut Behavior, bool),
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::switch(
            {
                let shell = shell.clone();
                move |app| read(&behavior_of(&shell, app))
            },
            {
                let shell = shell.clone();
                move |value, app| {
                    if let Some(shell) = shell.upgrade() {
                        shell.update(app, |shell, context| {
                            shell.edit_behavior(|behavior| write(behavior, value), context);
                        });
                    }
                }
            },
        ),
    )
}

/// The shell's current behavior settings, or the defaults once it has closed.
fn behavior_of(shell: &WeakEntity<WindowShell>, app: &App) -> Behavior {
    let settings = shell.upgrade().map_or_else(Settings::default, |entity| {
        entity.read(app).settings().clone()
    });
    Behavior {
        clipboard_write: settings.clipboard_write,
        confirm_multiline_paste: settings.confirm_multiline_paste,
        option_as_meta: settings.option_as_meta,
    }
}

/// Every registered theme's name, sorted for the dropdown's option list.
fn theme_options(app: &App) -> Vec<(SharedString, SharedString)> {
    ThemeRegistry::global(app)
        .sorted_themes()
        .into_iter()
        .map(|config| (config.name.clone(), config.name.clone()))
        .collect()
}

/// Every font family the platform text system knows, for the dropdown's option list.
fn font_options(app: &App) -> Vec<(SharedString, SharedString)> {
    app.text_system()
        .all_font_names()
        .into_iter()
        .map(|name| {
            let name = SharedString::from(name);
            (name.clone(), name)
        })
        .collect()
}

/// The name of the theme currently active for the kit's active mode.
fn active_theme_name(app: &App) -> SharedString {
    let theme = Theme::global(app);
    if theme.mode.is_dark() {
        theme.dark_theme.name.clone()
    } else {
        theme.light_theme.name.clone()
    }
}

/// Switch the active kit theme and re-derive the terminal's colors from it,
/// refreshing every open window so the change is visible immediately.
fn select_theme(shell: &WeakEntity<WindowShell>, name: &SharedString, app: &mut App) {
    if !crate::theme::apply_named(name, app) {
        return;
    }
    let colors = Theme::global(app).colors;
    edit_theme(shell, app, |theme| {
        theme.foreground = crate::theme::rgb_color(colors.foreground);
        theme.background = crate::theme::rgb_color(colors.background);
    });
}

/// The Keybindings page: the closed inventory's current chords, read-only for now.
fn keybindings_page(shell: &WeakEntity<WindowShell>) -> SettingPage {
    SettingPage::new("Keybindings").group(SettingGroup::new().items(INVENTORY.iter().map(
        |specification| {
            let action = format!("{:?}", specification.id);
            let default_chord = specification.keybinding.unwrap_or(NO_CHORD).to_owned();
            let shell = shell.clone();
            SettingItem::new(
                specification.name,
                SettingField::input(
                    move |app| {
                        let overridden = shell.upgrade().and_then(|entity| {
                            entity
                                .read(app)
                                .settings()
                                .keybindings
                                .get(&action)
                                .cloned()
                        });
                        SharedString::from(overridden.unwrap_or_else(|| default_chord.clone()))
                    },
                    |_value, _app| {},
                ),
            )
            .disabled(true)
        },
    )))
}
