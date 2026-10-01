//! Shared application theme values for GPUI and the terminal emulator.

use gpui_kit::component::{Theme, ThemeColor, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Hsla, Pixels, SharedString};
use libghostty_vt::style::Palette;

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

/// Field, text, and border for a failure strip, taken from `theme`.
///
/// The field is that theme's secondary surface painted over its background,
/// the same chrome the connection strip uses, so a light theme stays light
/// and a dark theme stays dark. The text is the danger colour from the same
/// theme that can be read on that field: `danger` where that token is the
/// accent, and `danger.foreground` where `danger` is itself a dark fill.
/// The kit's error alert is not used for these colours. It mixes `danger`
/// with transparent white, and that mix leaves the field nearly transparent.
#[must_use]
pub fn failure_strip_colors(theme: &ThemeColor) -> (Hsla, Hsla, Hsla) {
    let field = theme.background.blend(theme.secondary);
    (field, danger_text(theme, field), theme.border)
}

/// The opaque danger colour that reads best on `field`.
///
/// Starts from the theme's own text, so a theme whose danger accent is too
/// close to its surface keeps the text the rest of that theme already uses.
fn danger_text(theme: &ThemeColor, field: Hsla) -> Hsla {
    let mut chosen = theme.foreground;
    let mut chosen_ratio = contrast_ratio(chosen, field);
    for candidate in [theme.danger, theme.danger_foreground] {
        if !candidate.is_opaque() {
            continue;
        }
        let ratio = contrast_ratio(candidate, field);
        if ratio > chosen_ratio {
            chosen = candidate;
            chosen_ratio = ratio;
        }
    }
    chosen
}

/// Offset WCAG adds to both luminances before dividing them.
const CONTRAST_OFFSET: f32 = 0.05;
/// A channel at or below this sRGB value is still on the linear segment.
const LINEAR_THRESHOLD: f32 = 0.040_45;
/// Divisor of the linear segment of the sRGB transfer.
const LINEAR_DIVISOR: f32 = 12.92;
/// Offset inside the gamma segment of the sRGB transfer.
const GAMMA_OFFSET: f32 = 0.055;
/// Scale of the gamma segment of the sRGB transfer.
const GAMMA_SCALE: f32 = 1.055;
/// Exponent of the gamma segment of the sRGB transfer.
const GAMMA_EXPONENT: f32 = 2.4;
/// Share of luminance WCAG assigns to red.
const LUMINANCE_RED: f32 = 0.2126;
/// Share of luminance WCAG assigns to green.
const LUMINANCE_GREEN: f32 = 0.7152;
/// Share of luminance WCAG assigns to blue.
const LUMINANCE_BLUE: f32 = 0.0722;

/// The WCAG contrast ratio of two colours, from their relative luminance.
fn contrast_ratio(left: Hsla, right: Hsla) -> f32 {
    let left = luminance(left);
    let right = luminance(right);
    let (lighter, darker) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    (lighter + CONTRAST_OFFSET) / (darker + CONTRAST_OFFSET)
}

/// The relative luminance of a colour, as WCAG defines it.
fn luminance(colour: Hsla) -> f32 {
    let rgb = gpui_kit::Rgba::from(colour);
    let red = linear_channel(rgb.r);
    let green = linear_channel(rgb.g);
    let blue = linear_channel(rgb.b);
    (LUMINANCE_RED * red) + (LUMINANCE_GREEN * green) + (LUMINANCE_BLUE * blue)
}

/// One sRGB channel converted to linear light.
fn linear_channel(value: f32) -> f32 {
    if value <= LINEAR_THRESHOLD {
        value / LINEAR_DIVISOR
    } else {
        ((value + GAMMA_OFFSET) / GAMMA_SCALE).powf(GAMMA_EXPONENT)
    }
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

/// Convert the application theme into the emulator's source of truth, with
/// the emulator's own palette and the light or dark scheme its background is.
#[must_use]
pub fn terminal_theme(theme: &AppTheme) -> TerminalTheme {
    TerminalTheme {
        foreground: theme.foreground,
        background: theme.background,
        scheme: crate::vt::scheme_for(theme.background),
        ..TerminalTheme::default()
    }
}

/// The same, with the sixteen ANSI colors taken from a kit theme's colors, so
/// a program's red is the theme's red.
#[must_use]
pub fn terminal_theme_from(theme: &AppTheme, colors: &ThemeColor) -> TerminalTheme {
    TerminalTheme {
        palette: Some(ansi_palette(colors)),
        ..terminal_theme(theme)
    }
}

/// The terminal theme for `theme` under the kit theme active in `app`, or
/// with the emulator's own palette when no kit theme is installed.
#[must_use]
pub fn terminal_theme_in(theme: &AppTheme, app: &App) -> TerminalTheme {
    if app.has_global::<Theme>() {
        terminal_theme_from(theme, &Theme::global(app).colors)
    } else {
        terminal_theme(theme)
    }
}

/// Ghostty's 256-color palette with the six hues and their bright forms —
/// indexes one to six and nine to fourteen — replaced by the kit theme's own.
/// A theme's light variant is used for the bright form only when it is
/// opaque, since some themes make it a translucent tint for backgrounds.
/// Black, white and the grays keep Ghostty's values.
#[must_use]
pub fn ansi_palette(colors: &ThemeColor) -> Palette {
    let mut palette = Palette::default();
    let hues = [
        (colors.red, colors.red_light),
        (colors.green, colors.green_light),
        (colors.yellow, colors.yellow_light),
        (colors.blue, colors.blue_light),
        (colors.magenta, colors.magenta_light),
        (colors.cyan, colors.cyan_light),
    ];
    for (offset, (base, light)) in hues.into_iter().enumerate() {
        let bright = if light.a >= 1.0 { light } else { base };
        if let Some(slot) = offset
            .checked_add(1)
            .and_then(|index| palette.0.get_mut(index))
        {
            *slot = rgb_color(base);
        }
        if let Some(slot) = offset
            .checked_add(BRIGHT_RED)
            .and_then(|index| palette.0.get_mut(index))
        {
            *slot = rgb_color(bright);
        }
    }
    palette
}

/// Palette index of bright red, the first bright hue.
const BRIGHT_RED: usize = 9;

/// Bit position of a `0xRRGGBBAA` color's red channel.
const RED_SHIFT: u32 = 24;
/// Bit position of a `0xRRGGBBAA` color's green channel.
const GREEN_SHIFT: u32 = 16;
/// Bit position of a `0xRRGGBBAA` color's blue channel.
const BLUE_SHIFT: u32 = 8;
/// Mask isolating one byte of a color channel.
const CHANNEL_MASK: u32 = 0xFF;

/// Convert a kit `Hsla` color into the terminal's `RgbColor` byte triple.
#[must_use]
pub fn rgb_color(color: Hsla) -> RgbColor {
    let value = u32::from(color.to_rgb());
    RgbColor {
        r: channel(value, RED_SHIFT),
        g: channel(value, GREEN_SHIFT),
        b: channel(value, BLUE_SHIFT),
    }
}

/// One byte of a `0xRRGGBBAA` color, shifted into place.
fn channel(value: u32, shift: u32) -> u8 {
    u8::try_from((value >> shift) & CHANNEL_MASK).unwrap_or(u8::MAX)
}

/// Make a registered theme the active one, dark or light as that theme is.
///
/// Returns whether `name` is registered. An unknown name leaves the active
/// theme untouched.
pub fn apply_named(name: &str, app: &mut App) -> bool {
    let Some(config) = ThemeRegistry::global(app).themes().get(name).cloned() else {
        return false;
    };
    if !app.has_global::<Theme>() {
        return false;
    }
    let mode = config.mode;
    if mode.is_dark() {
        Theme::global_mut(app).dark_theme = config;
    } else {
        Theme::global_mut(app).light_theme = config;
    }
    Theme::change(mode, None, app);
    app.refresh_windows();
    true
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
