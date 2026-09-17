//! The settings window: a second OS window listing application settings.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::Settings as SettingsPanel;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{Root, Theme, ThemeRegistry, TitleBar};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Hsla, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, TestSupportExt, WeakEntity, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use libghostty_vt::style::RgbColor;

use crate::actions::INVENTORY;
use crate::theme::AppTheme;
use crate::window::WindowShell;

/// Lower bound a font size is clamped to.
const MIN_FONT_SIZE: f32 = 8.0;
/// Upper bound a font size is clamped to.
const MAX_FONT_SIZE: f32 = 32.0;

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
                        .page(keybindings_page(&self.shell)),
                ),
            )
    }
}

/// Read the shell's current theme, or the default when the main window closed.
fn theme_of(shell: &WeakEntity<WindowShell>, app: &App) -> AppTheme {
    shell.upgrade().map_or_else(AppTheme::default, |entity| {
        entity.read(app).settings.theme.clone()
    })
}

/// Apply a theme change to the live shell, when it still exists.
fn edit_theme(shell: &WeakEntity<WindowShell>, app: &mut App, edit: impl FnOnce(&mut AppTheme)) {
    let Some(shell) = shell.upgrade() else {
        return;
    };
    shell.update(app, |shell, context| {
        let mut theme = shell.settings.theme.clone();
        edit(&mut theme);
        shell.settings.theme = theme.clone();
        shell.apply_theme(&theme, context);
    });
}

/// The Appearance settings page: theme selection and terminal typography.
fn appearance_page(shell: &WeakEntity<WindowShell>, current: &App) -> SettingPage {
    SettingPage::new("Appearance").group(
        SettingGroup::new()
            .title("Appearance")
            .item(SettingItem::new(
                "Theme",
                SettingField::dropdown(theme_options(current), active_theme_name, {
                    let shell = shell.clone();
                    move |name, app| select_theme(&shell, &name, app)
                }),
            ))
            .item(SettingItem::new(
                "Font Family",
                SettingField::input(
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
                SettingField::render({
                    let shell = shell.clone();
                    move |_options, window, app| font_size_field(&shell, window, app)
                }),
            )),
    )
}

/// Every registered theme's name, sorted for the dropdown's option list.
fn theme_options(app: &App) -> Vec<(SharedString, SharedString)> {
    ThemeRegistry::global(app)
        .sorted_themes()
        .into_iter()
        .map(|config| (config.name.clone(), config.name.clone()))
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
    let Some(config) = ThemeRegistry::global(app).themes().get(name).cloned() else {
        return;
    };
    let mode = config.mode;
    if mode.is_dark() {
        Theme::global_mut(app).dark_theme = config;
    } else {
        Theme::global_mut(app).light_theme = config;
    }
    Theme::change(mode, None, app);
    app.refresh_windows();
    let colors = Theme::global(app).colors;
    edit_theme(shell, app, |theme| {
        theme.foreground = rgb_color(colors.foreground);
        theme.background = rgb_color(colors.background);
    });
}

/// A font-size input's persisted state: its text field and the subscription
/// that applies and clamps its value.
struct FontSizeField {
    /// The text field the user types into.
    input: Entity<InputState>,
    /// Kept alive so the field keeps reacting to input events.
    _subscription: Subscription,
}

/// A font-size input that applies live as typed and, once the user finishes
/// editing, clamps the value to `MIN_FONT_SIZE..=MAX_FONT_SIZE` and normalizes
/// the displayed text. Clamping on every keystroke instead would overwrite a
/// number still being typed (an in-progress "14" reads as "1", clamps to the
/// minimum, and erases what was typed), so bounds apply live to the theme but
/// only rewrite the field's own text on blur or Enter.
fn font_size_field(shell: &WeakEntity<WindowShell>, window: &mut Window, app: &mut App) -> Input {
    let initial = theme_of(shell, app).font_size;
    let state = window.use_keyed_state(SharedString::from("appearance-font-size"), app, {
        let shell = shell.clone();
        move |setup_window, setup_cx| {
            let field_input = setup_cx.new(|input_cx| {
                InputState::new(setup_window, input_cx).default_value(initial.to_string())
            });
            let subscription = setup_cx.subscribe_in(&field_input, setup_window, {
                let shell = shell.clone();
                move |_state: &mut FontSizeField,
                      event_input,
                      event: &InputEvent,
                      event_window,
                      event_cx| {
                    apply_font_size_event(&shell, event_input, event, event_window, event_cx);
                }
            });
            FontSizeField {
                input: field_input,
                _subscription: subscription,
            }
        }
    });
    Input::new(&state.read(app).input)
}

/// Apply a font-size field's event: live on every change, clamped and
/// normalized once the user finishes editing.
fn apply_font_size_event(
    shell: &WeakEntity<WindowShell>,
    input: &Entity<InputState>,
    event: &InputEvent,
    window: &mut Window,
    app: &mut App,
) {
    match event {
        InputEvent::Change => {
            if let Ok(font_size) = input.read(app).value().parse::<f32>() {
                let clamped = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                edit_theme(shell, app, |theme| theme.font_size = clamped);
            }
        }
        InputEvent::Blur | InputEvent::PressEnter { .. } => {
            if let Ok(font_size) = input.read(app).value().parse::<f32>() {
                let clamped = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                edit_theme(shell, app, |theme| theme.font_size = clamped);
                input.update(app, |input, cx| {
                    input.set_value(SharedString::from(clamped.to_string()), window, cx);
                });
            }
        }
        InputEvent::Focus => {}
    }
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
                            entity.read(app).settings.keybindings.get(&action).cloned()
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

/// Convert a kit `Hsla` color into the terminal's `RgbColor` byte triple.
fn rgb_color(color: Hsla) -> RgbColor {
    let packed = u32::from(color.to_rgb());
    RgbColor {
        r: channel(packed, 24),
        g: channel(packed, 16),
        b: channel(packed, 8),
    }
}

/// One byte of a packed `0xRRGGBBAA` color, shifted into place.
fn channel(packed: u32, shift: u32) -> u8 {
    u8::try_from((packed >> shift) & 0xFF).unwrap_or(u8::MAX)
}
