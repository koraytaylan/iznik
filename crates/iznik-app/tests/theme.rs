//! The bundled Ayu Mirage theme registers and becomes the application default.

use gpui_kit::TestAppContext;
use gpui_kit::component::{ActiveTheme, ThemeMode};
use iznik_app::theme::apply_default_theme;

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

/// Register the bundled theme and assert it becomes the active dark theme.
///
/// # Errors
/// Returns the registration failure, or a mismatched mode or name.
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
        Ok(())
    })
}
