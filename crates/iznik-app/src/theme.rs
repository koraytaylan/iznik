//! Shared application theme values for GPUI and the terminal emulator.

use crate::vt::TerminalTheme;

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
