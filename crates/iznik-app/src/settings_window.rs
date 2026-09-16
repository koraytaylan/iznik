//! The settings window: a second OS window listing application settings.

use gpui_kit::component::Root;
use gpui_kit::component::setting::Settings as SettingsPanel;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::{
    App, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, TestSupportExt, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowOptions, div, px, size,
};

use crate::actions::INVENTORY;
use crate::theme::AppTheme;
use crate::window::WindowShell;

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
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .id("settings-window-content")
            .test_support()
            .size_full()
            .child(
                SettingsPanel::new("iznik-settings")
                    .page(theme_page(&self.shell))
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

/// The Theme settings page: foreground, background, font family and size.
fn theme_page(shell: &WeakEntity<WindowShell>) -> SettingPage {
    SettingPage::new("Theme").group(
        SettingGroup::new()
            .title("Colors")
            .item(SettingItem::new(
                "Foreground",
                SettingField::input(
                    {
                        let shell = shell.clone();
                        move |app| color_text(theme_of(&shell, app).foreground)
                    },
                    {
                        let shell = shell.clone();
                        move |value, app| {
                            if let Some(color) = parse_color(&value) {
                                edit_theme(&shell, app, |theme| theme.foreground = color);
                            }
                        }
                    },
                ),
            ))
            .item(SettingItem::new(
                "Background",
                SettingField::input(
                    {
                        let shell = shell.clone();
                        move |app| color_text(theme_of(&shell, app).background)
                    },
                    {
                        let shell = shell.clone();
                        move |value, app| {
                            if let Some(color) = parse_color(&value) {
                                edit_theme(&shell, app, |theme| theme.background = color);
                            }
                        }
                    },
                ),
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
                            if let Ok(font_size) = value.parse() {
                                edit_theme(&shell, app, |theme| theme.font_size = font_size);
                            }
                        }
                    },
                ),
            )),
    )
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

/// Format a color as the `r,g,b` text the settings file itself uses.
fn color_text(color: libghostty_vt::style::RgbColor) -> SharedString {
    SharedString::from(format!("{},{},{}", color.r, color.g, color.b))
}

/// Parse a `r,g,b` color, accepting only well-formed byte components.
fn parse_color(value: &str) -> Option<libghostty_vt::style::RgbColor> {
    let mut parts = value.split(',');
    let red = parts.next()?.trim().parse().ok()?;
    let green = parts.next()?.trim().parse().ok()?;
    let blue = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(libghostty_vt::style::RgbColor {
        r: red,
        g: green,
        b: blue,
    })
}
