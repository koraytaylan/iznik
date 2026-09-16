//! The settings window: a second OS window listing application settings.

use gpui_kit::component::setting::Settings as SettingsPanel;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{Root, Theme, ThemeRegistry};
use gpui_kit::{
    App, AppContext as _, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, TestSupportExt, TitlebarOptions, WeakEntity, Window, WindowBounds,
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
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(WINDOW_TITLE)),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            context,
        )),
        ..Default::default()
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
            .child(
                SettingsPanel::new("iznik-settings")
                    .page(appearance_page(&self.shell, context))
                    .page(keybindings_page(&self.shell)),
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
                SettingField::input(
                    {
                        let shell = shell.clone();
                        move |app| SharedString::from(theme_of(&shell, app).font_size.to_string())
                    },
                    {
                        let shell = shell.clone();
                        move |value, app| {
                            if let Ok(font_size) = value.parse::<f32>() {
                                let clamped = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                                edit_theme(&shell, app, |theme| theme.font_size = clamped);
                            }
                        }
                    },
                ),
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
