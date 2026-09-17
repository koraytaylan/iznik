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
/// Default row height as a multiple of the font size: the conventional
/// comfortable spacing (CSS's `normal` for most fonts, Windows Terminal's
/// default), which keeps descenders clear of the row below.
const DEFAULT_LINE_HEIGHT: f32 = 1.2;
/// The family name that means "the system's monospace font" rather than any
/// one font.
pub const GENERIC_MONOSPACE: &str = "monospace";
/// Monospace families tried, in order, when the generic family is asked for
/// or the requested family is not installed: common programming fonts first,
/// then each platform's own.
const MONOSPACE_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "Source Code Pro",
    "SF Mono",
    "Menlo",
    "Consolas",
    "Ubuntu Sans Mono",
    "Ubuntu Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Courier New",
];
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
    /// Row height as a multiple of the font size.
    pub line_height: f32,
    /// Whether the selected session's tabs are drawn in the window's title
    /// bar rather than in a bar of their own.
    pub tabs_in_title_bar: bool,
}

impl Default for AppTheme {
    fn default() -> Self {
        let terminal = TerminalTheme::default();
        Self {
            foreground: terminal.foreground,
            background: terminal.background,
            font_family: GENERIC_MONOSPACE.to_owned(),
            font_size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
            tabs_in_title_bar: false,
        }
    }
}

/// The family the terminal draws with: the requested one when it is
/// installed, otherwise the first installed common monospace family, and the
/// request itself when none is — the text system's own fallback is then all
/// there is.
///
/// A terminal places every glyph on a column grid, so a proportional font
/// draws uneven text; the generic name is not one the text system resolves to
/// a monospace face by itself.
#[must_use]
pub fn terminal_font(requested: &str, installed: &[String]) -> String {
    let is_installed = |family: &str| installed.iter().any(|name| name == family);
    if requested != GENERIC_MONOSPACE && is_installed(requested) {
        return requested.to_owned();
    }
    MONOSPACE_FAMILIES
        .iter()
        .find(|family| is_installed(family))
        .map_or_else(|| requested.to_owned(), |family| (*family).to_owned())
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
