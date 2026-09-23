//! The terminal takes its ANSI colors from the kit theme and tells programs
//! whether it is light or dark from its background.

use gpui_kit::TestAppContext;
use gpui_kit::component::Theme;
use iznik_app::theme::{
    AppTheme, apply_default_theme, rgb_color, terminal_theme, terminal_theme_in,
};
use libghostty_vt::style::{Palette, RgbColor};
use libghostty_vt::terminal::ColorScheme;

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;

/// A dark background reads as dark and a light one as light.
///
/// # Panics
/// Panics when the scheme does not follow the background.
#[test]
fn scheme_follows_the_background() {
    let dark = AppTheme::default();
    assert_eq!(terminal_theme(&dark).scheme, ColorScheme::Dark);
    let light = AppTheme {
        background: RgbColor {
            r: 250,
            g: 248,
            b: 240,
        },
        ..AppTheme::default()
    };
    assert_eq!(terminal_theme(&light).scheme, ColorScheme::Light);
}

/// The kit theme's red is the terminal's palette red.
#[gpui_kit::test]
fn palette_takes_the_kit_theme_hues(context: &mut TestAppContext) {
    check(&hues(context));
}

/// Install the bundled themes and compare the terminal palette with them.
///
/// # Errors
/// Returns the theme registration failure or a missing palette.
///
/// # Panics
/// Panics when a hue is not the kit theme's.
fn hues(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| {
        gpui_kit::init(app);
        apply_default_theme(app)?;
        let colors = Theme::global(app).colors;
        let terminal = terminal_theme_in(&AppTheme::default(), app);
        let palette = terminal.palette.ok_or("no palette")?;
        assert_eq!(palette.0[1], rgb_color(colors.red), "red");
        assert_eq!(palette.0[4], rgb_color(colors.blue), "blue");
        assert_eq!(palette.0[0], Palette::default().0[0], "black kept");
        Ok::<_, Failed>(())
    })
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}
