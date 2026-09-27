//! The bundled themes register and Ayu Mirage becomes the application default.

use gpui_kit::TestAppContext;
use gpui_kit::component::{ActiveTheme, ThemeMode, ThemeRegistry};
use iznik_app::theme::{
    GENERIC_MONOSPACE, apply_default_theme, apply_named, failure_strip_colors, terminal_font,
};

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

/// The failure strip's text is legible on an opaque field from the theme
/// that is selected, for every registered theme and not only the default.
///
/// The kit's error alert mixes `danger` with transparent white at a small
/// factor, and that mix weights the first colour by the factor, so the field
/// is nearly transparent. Painting `danger` itself as the field only matches
/// a theme that defined that token as a panel. The strip therefore uses the
/// selected theme's own surface, and this holds every registered theme to an
/// opaque field and text that reads on it.
#[gpui_kit::test]
fn the_failure_banner_colour_is_readable(context: &mut TestAppContext) {
    check(&contrast(context));
}

/// Apply every registered theme and assert each failure strip's field is
/// that theme's opaque surface and its text reads on it.
///
/// # Errors
/// Returns the registration failure, a theme that did not become active, a
/// transparent field, a contrast failure naming the theme and the ratio, or
/// two themes whose strips share one field.
fn contrast(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| -> Result<(), Failed> {
        gpui_kit::init(app);
        apply_default_theme(app)?;
        let names: Vec<_> = ThemeRegistry::global(app)
            .themes()
            .keys()
            .cloned()
            .collect();
        let mut fields = Vec::new();
        for name in &names {
            fields.push(one_theme(app, name)?);
        }
        distinct(&fields)
    })
}

/// Select `name` and return its failure-strip field when the strip is readable.
///
/// # Errors
/// Returns a theme that did not become active, a transparent field, or a
/// contrast failure naming the theme and the ratio.
fn one_theme(app: &mut gpui_kit::App, name: &str) -> Result<gpui_kit::Hsla, Failed> {
    if !apply_named(name, app) {
        return Err(format!("{name} is registered but could not be selected").into());
    }
    let theme = app.theme();
    if theme.theme_name().as_ref() != name {
        return Err(format!("{name} did not become the active theme").into());
    }
    let (field, text, _) = failure_strip_colors(theme);
    let surface = theme.background.blend(theme.secondary);
    if field != surface {
        return Err(format!("{name}: the failure strip does not use that theme's surface").into());
    }
    if field.a < 1.0 {
        return Err(format!(
            "{name}: the failure strip's field is transparent (alpha {})",
            field.a
        )
        .into());
    }
    let ratio = contrast_ratio(text, field);
    let body = contrast_ratio(theme.foreground, field);
    let required = if body < MINIMUM_CONTRAST {
        body
    } else {
        MINIMUM_CONTRAST
    };
    if ratio < required {
        return Err(format!(
            "{name}: failure strip text on its field is {ratio:.2}:1, below the \
             {required:.2}:1 that theme can read"
        )
        .into());
    }
    Ok(field)
}

/// Two themes must not be given the same failure field.
///
/// # Errors
/// Returns when every field is one colour, which is a strip that ignores the
/// selected theme.
fn distinct(fields: &[gpui_kit::Hsla]) -> Result<(), Failed> {
    let Some(first) = fields.first() else {
        return Err("no theme was registered".into());
    };
    if fields.iter().all(|field| field == first) {
        return Err("every theme paints the same failure strip".into());
    }
    Ok(())
}

/// The fewest contrast a banner's text may have, the WCAG AA threshold for
/// body text.
const MINIMUM_CONTRAST: f32 = 4.5;

/// The WCAG contrast ratio of two opaque colours, from their relative
/// luminance.
fn contrast_ratio(left: gpui_kit::Hsla, right: gpui_kit::Hsla) -> f32 {
    let left = luminance(left);
    let right = luminance(right);
    let (lighter, darker) = if left > right {
        (left, right)
    } else {
        (right, left)
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// The relative luminance of a colour, as WCAG defines it.
fn luminance(colour: gpui_kit::Hsla) -> f32 {
    let rgb = gpui_kit::Rgba::from(colour);
    let channel = |value: f32| {
        if value <= 0.040_45 {
            value / 12.92
        } else {
            // The sRGB transfer function, with its two named constants.
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let red = channel(rgb.r);
    let green = channel(rgb.g);
    let blue = channel(rgb.b);
    // The luminosity coefficients WCAG names.
    (0.2126 * red) + (0.7152 * green) + (0.0722 * blue)
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
