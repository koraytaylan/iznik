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

/// The failure banner's colour — `danger.foreground` — is legible on the
/// alert's background, which is built from `danger`.
///
/// The kit's error alert paints its message in `theme.danger`, the
/// `danger.background` token: a colour meant to sit *behind* text. Ayu Mirage
/// declares it as a dark red, so the alert was dark red on near-dark red — a
/// contrast ratio of 1.1, which is not text a person can read. The banner
/// therefore draws in `danger.foreground`, and this holds the application's
/// own theme to those two tokens being far enough apart to read.
#[gpui_kit::test]
fn the_failure_banner_colour_is_readable(context: &mut TestAppContext) {
    check(&contrast(context));
}

/// Apply the default theme and assert its danger foreground reads on its
/// danger background.
///
/// # Errors
/// Returns the registration failure, or the contrast failure naming the ratio.
fn contrast(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| -> Result<(), Failed> {
        gpui_kit::init(app);
        apply_default_theme(app)?;
        let theme = app.theme();
        // The alert's own background is a small mix of the danger colour
        // toward white, which is what the text must read against.
        let background = mix_toward_white(theme.danger, DANGER_MIX);
        let ratio = contrast_ratio(theme.danger_foreground, background);
        if ratio < MINIMUM_CONTRAST {
            return Err(format!(
                "{}: danger foreground on its alert background is {ratio:.2}:1, below the \
                 {MINIMUM_CONTRAST}:1 a person can read",
                theme.theme_name()
            )
            .into());
        }
        Ok(())
    })
}

/// The share of white the kit mixes into a variant's colour for its
/// background. `AlertVariant::bg` mixes `transparent_white` at 0.04.
const DANGER_MIX: f32 = 0.04;

/// The fewest contrast a banner's text may have, the WCAG AA threshold for
/// body text.
const MINIMUM_CONTRAST: f32 = 4.5;

/// `colour` mixed `share` of the way toward white, as the kit's alert does.
fn mix_toward_white(colour: gpui_kit::Hsla, share: f32) -> gpui_kit::Hsla {
    let white = gpui_kit::white();
    gpui_kit::Hsla {
        h: colour.h,
        s: colour.s * (1.0 - share),
        l: colour.l + (white.l - colour.l) * share,
        a: 1.0,
    }
}

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
