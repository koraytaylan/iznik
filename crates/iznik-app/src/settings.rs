//! Validated, hot-reloadable application settings.

use crate::actions::INVENTORY;
use crate::theme::AppTheme;
use libghostty_vt::style::RgbColor;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::SystemTime;

/// Settings held by the application after validation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Shared terminal and GPUI theme.
    pub theme: AppTheme,
    /// Keybinding overrides keyed by action name.
    pub keybindings: BTreeMap<String, String>,
}

/// A settings update that preserves the previous values on refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    /// The field that failed validation.
    pub field: String,
    /// Human-readable reason.
    pub message: String,
}

/// A polling watcher for the application settings file.
#[derive(Clone, Debug)]
pub struct Watcher {
    /// File whose changes are observed.
    pub path: PathBuf,
    /// Modification stamp last applied.
    stamp: Option<SystemTime>,
}

impl Watcher {
    /// Create a watcher that has not applied any file yet.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stamp: None,
        }
    }

    /// Reload a changed file, returning whether a new settings value was applied.
    ///
    /// # Errors
    ///
    /// Returns I/O or field-specific decoding errors while leaving `current` unchanged.
    pub fn reload(&mut self, current: &mut Settings) -> Result<bool, SettingsError> {
        let metadata = std::fs::metadata(&self.path)
            .map_err(|io_error| error("file", &io_error.to_string()))?;
        let modified = metadata
            .modified()
            .map_err(|io_error| error("file", &io_error.to_string()))?;
        if self.stamp == Some(modified) {
            return Ok(false);
        }
        let candidate = decode(
            &std::fs::read_to_string(&self.path)
                .map_err(|io_error| error("file", &io_error.to_string()))?,
        )?;
        replace(current, candidate)?;
        self.stamp = Some(modified);
        Ok(true)
    }
}

/// Apply a validated update, retaining the old settings when validation fails.
///
/// # Errors
///
/// Returns an error naming `keybindings` when an action name is empty.
pub fn replace(current: &mut Settings, candidate: Settings) -> Result<(), SettingsError> {
    validate_keybindings(&candidate.keybindings)?;
    *current = candidate;
    Ok(())
}

/// Validate action names and chord uniqueness against the closed inventory.
///
/// # Errors
///
/// Returns a field-specific error for an unknown action, empty action, or
/// colliding chord.
pub fn validate_keybindings(keybindings: &BTreeMap<String, String>) -> Result<(), SettingsError> {
    let mut chords = BTreeMap::new();
    for (action, chord) in keybindings {
        if action.trim().is_empty() {
            return Err(error("keybindings", "action name is empty"));
        }
        if !INVENTORY
            .iter()
            .any(|specification| format!("{:?}", specification.id) == *action)
        {
            return Err(error(action, "action is not registered"));
        }
        if chords.insert(chord, action).is_some() {
            return Err(error(action, "keybinding chord collides"));
        }
    }
    Ok(())
}

/// Serialize settings into the stable line format used by the application file.
#[must_use]
pub fn encode(settings: &Settings) -> String {
    let mut text = format!(
        "foreground={},{},{}\nbackground={},{},{}\nfont_family={}\nfont_size={}\nline_height={}\ntabs_in_title_bar={}\n",
        settings.theme.foreground.r,
        settings.theme.foreground.g,
        settings.theme.foreground.b,
        settings.theme.background.r,
        settings.theme.background.g,
        settings.theme.background.b,
        settings.theme.font_family,
        settings.theme.font_size,
        settings.theme.line_height,
        settings.theme.tabs_in_title_bar
    );
    for (action, chord) in &settings.keybindings {
        let _written = writeln!(text, "keybinding.{action}={chord}");
    }
    text
}

/// Parse settings while naming the malformed field and preserving no partial result.
///
/// # Errors
///
/// Returns a field-specific error for malformed colors, font size, or lines.
pub fn decode(text: &str) -> Result<Settings, SettingsError> {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((field, value)) = line.split_once('=') else {
            return Err(error("file", "line has no equals sign"));
        };
        match field {
            "foreground" => settings.theme.foreground = color(field, value)?,
            "background" => settings.theme.background = color(field, value)?,
            "font_family" => value.clone_into(&mut settings.theme.font_family),
            "tabs_in_title_bar" => {
                settings.theme.tabs_in_title_bar = value
                    .parse()
                    .map_err(|_parse_error| error(field, "not true or false"))?;
            }
            "line_height" => {
                settings.theme.line_height = value
                    .parse()
                    .map_err(|_parse_error| error(field, "not a number"))?;
            }
            "font_size" => {
                settings.theme.font_size = value
                    .parse()
                    .map_err(|_parse_error| error(field, "not a number"))?;
            }
            key if key.starts_with("keybinding.") => {
                let action = key.trim_start_matches("keybinding.");
                if action.is_empty() {
                    return Err(error(field, "action name is empty"));
                }
                settings
                    .keybindings
                    .insert(action.to_owned(), value.to_owned());
            }
            _ => return Err(error(field, "unknown field")),
        }
    }
    Ok(settings)
}

/// Parse one RGB color value.
///
/// # Errors
///
/// Returns a field-specific error when the value is not three byte components.
fn color(field: &str, value: &str) -> Result<RgbColor, SettingsError> {
    let parts: Vec<_> = value.split(',').collect();
    let [red, green, blue] = parts.as_slice() else {
        return Err(error(field, "expected red,green,blue"));
    };
    let parse = |component: &str| {
        component
            .parse()
            .map_err(|_parse_error| error(field, "color component is not a byte"))
    };
    Ok(RgbColor {
        r: parse(red)?,
        g: parse(green)?,
        b: parse(blue)?,
    })
}

/// Construct a field-specific settings error.
fn error(field: &str, message: &str) -> SettingsError {
    SettingsError {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

impl crate::window::WindowShell {
    /// The settings the shell is running with.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}
