//! Shared application theme values for GPUI and the terminal emulator.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Pixels, SharedString};

use crate::vt::TerminalTheme;

/// The bundled Ayu Mirage theme, embedded so no external file is required.
const AYU_MIRAGE_THEME: &str = include_str!("../assets/ayu-mirage-theme.json");
/// The Ayu Mirage theme's name, exactly as declared in [`AYU_MIRAGE_THEME`].
const AYU_MIRAGE_THEME_NAME: &str = "Ayu Mirage";

/// Every other bundled theme family, taken from gpui-kit's own theme
/// collection and already in its native schema, so each registers without
/// any conversion from a different theme format.
const BUNDLED_THEME_FAMILIES: &[&str] = &[
    include_str!("../assets/themes/adventure.json"),
    include_str!("../assets/themes/alduin.json"),
    include_str!("../assets/themes/asciinema.json"),
    include_str!("../assets/themes/aurora.json"),
    include_str!("../assets/themes/ayu.json"),
    include_str!("../assets/themes/catppuccin.json"),
    include_str!("../assets/themes/everforest.json"),
    include_str!("../assets/themes/fahrenheit.json"),
    include_str!("../assets/themes/flexoki.json"),
    include_str!("../assets/themes/gruvbox.json"),
    include_str!("../assets/themes/harper.json"),
    include_str!("../assets/themes/hybrid.json"),
    include_str!("../assets/themes/jellybeans.json"),
    include_str!("../assets/themes/kibble.json"),
    include_str!("../assets/themes/macos-classic.json"),
    include_str!("../assets/themes/mellifluous.json"),
    include_str!("../assets/themes/molokai.json"),
    include_str!("../assets/themes/solarized.json"),
    include_str!("../assets/themes/spaceduck.json"),
    include_str!("../assets/themes/tokyonight.json"),
    include_str!("../assets/themes/twilight.json"),
];

/// Register every bundled theme and make Ayu Mirage the application's
/// default, replacing the kit's own light theme.
///
/// # Errors
///
/// Returns the registry's parse failure, or a message naming the theme when
/// it somehow failed to register under its own declared name.
pub fn apply_default_theme(app: &mut App) -> Result<(), String> {
    ThemeRegistry::global_mut(app)
        .load_themes_from_str(AYU_MIRAGE_THEME)
        .map_err(|error| error.to_string())?;
    for family in BUNDLED_THEME_FAMILIES {
        ThemeRegistry::global_mut(app)
            .load_themes_from_str(family)
            .map_err(|error| error.to_string())?;
    }
    let theme = ThemeRegistry::global(app)
        .themes()
        .get(AYU_MIRAGE_THEME_NAME)
        .cloned()
        .ok_or_else(|| format!("{AYU_MIRAGE_THEME_NAME} did not register under its own name"))?;
    Theme::global_mut(app).dark_theme = theme;
    Theme::change(ThemeMode::Dark, None, app);
    Ok(())
}

/// Default terminal font size in logical pixels.
const DEFAULT_FONT_SIZE: f32 = 14.0;
use libghostty_vt::style::RgbColor;

/// User-facing colors and typography shared by every surface.
#[derive(Clone, Debug, PartialEq)]
pub struct AppTheme {
    /// Default text color.
    pub foreground: RgbColor,
    /// Default background color.
    pub background: RgbColor,
    /// Font family used by the terminal grid.
    pub font_family: String,
    /// Font size in logical pixels.
    pub font_size: f32,
}

impl Default for AppTheme {
    fn default() -> Self {
        let terminal = TerminalTheme::default();
        Self {
            foreground: terminal.foreground,
            background: terminal.background,
            font_family: "monospace".to_owned(),
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

/// Convert the application theme into the emulator's source of truth.
#[must_use]
pub fn terminal_theme(theme: &AppTheme) -> TerminalTheme {
    TerminalTheme {
        foreground: theme.foreground,
        background: theme.background,
        ..TerminalTheme::default()
    }
}

/// Restyle the kit's own general and monospace UI text to match the
/// terminal's configured font, so a font change is visible in the
/// application's own chrome, not only in terminal content.
pub fn apply_chrome_font(app: &mut App, font: SharedString, size: Pixels) {
    let chrome_theme = Theme::global_mut(app);
    chrome_theme.font_family = font.clone();
    chrome_theme.font_size = size;
    chrome_theme.mono_font_family = font;
    chrome_theme.mono_font_size = size;
    app.refresh_windows();
}
