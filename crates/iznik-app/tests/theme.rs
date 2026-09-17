//! The bundled themes register and Ayu Mirage becomes the application default.

use gpui_kit::TestAppContext;
use gpui_kit::component::{ActiveTheme, ThemeMode, ThemeRegistry};
use iznik_app::theme::{GENERIC_MONOSPACE, apply_default_theme, terminal_font};

/// A representative name from each bundled theme family, present only if
/// every family actually registered, not only the default.
const REPRESENTATIVE_THEMES: &[&str] = &[
    "Ayu Dark",
    "Catppuccin Mocha",
    "Gruvbox Dark",
    "Solarized Light",
    "Tokyo Night",
];

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;

/// Applying the default theme selects Ayu Mirage in dark mode.
#[gpui_kit::test]
fn ayu_mirage_becomes_the_default_theme(context: &mut TestAppContext) {
    check(&applies(context));
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Register every bundled theme and assert Ayu Mirage becomes the active
/// dark theme, and that every other bundled family also registered.
///
/// # Errors
/// Returns the registration failure, a mismatched mode or name, or a
/// missing bundled family.
fn applies(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| -> Result<(), Failed> {
        gpui_kit::init(app);
        apply_default_theme(app)?;
        if app.theme().mode != ThemeMode::Dark {
            return Err("theme mode is not dark after applying Ayu Mirage".into());
        }
        if app.theme().theme_name().as_ref() != "Ayu Mirage" {
            return Err("active theme name is not Ayu Mirage".into());
        }
        for name in REPRESENTATIVE_THEMES {
            if !ThemeRegistry::global(app).themes().contains_key(*name) {
                return Err(format!("bundled theme family missing: {name}").into());
            }
        }
        Ok(())
    })
}

#[test]
/// The terminal draws with the requested family when it is installed, and
/// otherwise — the generic name included — with an installed common
/// monospace family.
///
/// # Panics
///
/// Panics when a resolved family differs.
fn the_terminal_font_is_an_installed_monospace_family() {
    let installed = [
        "Ubuntu Sans".to_owned(),
        "DejaVu Sans Mono".to_owned(),
        "Source Code Pro".to_owned(),
    ];
    assert_eq!(
        terminal_font(GENERIC_MONOSPACE, &installed),
        "Source Code Pro"
    );
    assert_eq!(
        terminal_font("DejaVu Sans Mono", &installed),
        "DejaVu Sans Mono"
    );
    assert_eq!(
        terminal_font("Not Installed", &installed),
        "Source Code Pro"
    );
    assert_eq!(terminal_font(GENERIC_MONOSPACE, &[]), GENERIC_MONOSPACE);
}
