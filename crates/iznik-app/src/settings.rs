//! Validated application settings, stored as one file and applied while running.
//!
//! The file lives at `$XDG_CONFIG_HOME/iznik/settings`, or
//! `~/.config/iznik/settings` when that variable is unset. A missing file is
//! the built-in default. A malformed file is refused, with the previous
//! values kept and the field named.

use gpui_kit::Context;
use gpui_kit::component::Theme;
use iznik_client::host::identity::HostId;
use libghostty_vt::style::RgbColor;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::actions::INVENTORY;
use crate::host_ui::{Notice, NoticeKind};
use crate::theme::AppTheme;
use crate::window::WindowShell;

/// The directory name under `$HOME` when `XDG_CONFIG_HOME` is unset.
const CONFIGURATION_DIRECTORY: &str = ".config";

/// The directory under the configuration home that holds this file.
const APPLICATION_DIRECTORY: &str = "iznik";

/// The file name of the settings.
const SETTINGS_FILE: &str = "settings";

/// The environment variable that names the configuration home.
const CONFIGURATION_HOME: &str = "XDG_CONFIG_HOME";

/// The environment variable that names the home directory.
const HOME: &str = "HOME";

/// The extension of the temporary file settings are written to before they
/// replace the file a launch reads.
const TEMPORARY_EXTENSION: &str = "temporary";

/// Settings held by the application after validation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Shared terminal and GPUI theme.
    pub theme: AppTheme,
    /// Name of the kit theme last chosen. Empty leaves the built-in default.
    pub theme_name: String,
    /// Keybinding overrides keyed by action name.
    pub keybindings: BTreeMap<String, String>,
}

/// The file this machine keeps: `$XDG_CONFIG_HOME/iznik/settings`, or
/// `~/.config/iznik/settings` when that variable is unset.
///
/// `None` when neither home is known, which is when there is nowhere to write.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    let directory = std::env::var_os(CONFIGURATION_HOME)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(HOME)
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(CONFIGURATION_DIRECTORY))
        })?;
    Some(directory.join(APPLICATION_DIRECTORY).join(SETTINGS_FILE))
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
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(io_error) => return Err(error("file", &io_error.to_string())),
        };
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

    /// Replace the settings file and remember its modification stamp.
    ///
    /// The stamp is the one just written, so the next poll does not apply the
    /// same change a second time. The previous file stays in place when the
    /// temporary file cannot be written.
    ///
    /// # Errors
    ///
    /// Returns a file error when the directory cannot be created or the file
    /// cannot be replaced.
    pub fn write(&mut self, settings: &Settings) -> Result<(), SettingsError> {
        write(&self.path, settings)?;
        let modified = std::fs::metadata(&self.path)
            .and_then(|metadata| metadata.modified())
            .map_err(|io_error| error("file", &io_error.to_string()))?;
        self.stamp = Some(modified);
        Ok(())
    }
}

/// Replace the settings at `path`, creating its directory when it is missing.
///
/// # Errors
///
/// Returns the operating system's error when the directory cannot be created
/// or the file cannot be replaced.
pub fn write(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|io_error| error("file", &io_error.to_string()))?;
    }
    let temporary = path.with_extension(TEMPORARY_EXTENSION);
    std::fs::write(&temporary, encode(settings))
        .map_err(|io_error| error("file", &io_error.to_string()))?;
    replace_file(&temporary, path)
}

/// Move `temporary` onto `path`. A rename that cannot replace an existing
/// file removes that file and tries once more.
///
/// # Errors
///
/// Returns a file error when the settings cannot be replaced.
fn replace_file(temporary: &Path, path: &Path) -> Result<(), SettingsError> {
    if std::fs::rename(temporary, path).is_ok() {
        return Ok(());
    }
    if path.is_file() {
        std::fs::remove_file(path).map_err(|io_error| error("file", &io_error.to_string()))?;
    }
    std::fs::rename(temporary, path).map_err(|io_error| error("file", &io_error.to_string()))
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
        "foreground={},{},{}\nbackground={},{},{}\nfont_family={}\nfont_size={}\nline_height={}\ntabs_in_title_bar={}\ntheme_name={}\n",
        settings.theme.foreground.r,
        settings.theme.foreground.g,
        settings.theme.foreground.b,
        settings.theme.background.r,
        settings.theme.background.g,
        settings.theme.background.b,
        settings.theme.font_family,
        settings.theme.font_size,
        settings.theme.line_height,
        settings.theme.tabs_in_title_bar,
        settings.theme_name
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
            "theme_name" => value.clone_into(&mut settings.theme_name),
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

impl WindowShell {
    /// The settings the shell is running with.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

/// Read a saved file into the shell, then apply it. A missing file leaves the
/// built-in default in place and is not an error.
pub(crate) fn load_into(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    let mut settings = shell.settings.clone();
    let outcome = {
        let Some(watcher) = shell.settings_watcher.as_mut() else {
            apply_saved(shell, context);
            return;
        };
        watcher.reload(&mut settings)
    };
    match outcome {
        Ok(true) => shell.settings = settings,
        Err(refusal) => refuse(shell, &refusal, context),
        Ok(false) => {}
    }
    apply_saved(shell, context);
}

/// Apply the shell's saved theme name and typography to every surface.
pub(crate) fn apply_saved(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    if !shell.settings.theme_name.is_empty()
        && !crate::theme::apply_named(&shell.settings.theme_name, context)
    {
        refuse(
            shell,
            &error("theme_name", "theme is not registered"),
            context,
        );
    }
    let theme = shell.settings.theme.clone();
    shell.apply_theme(&theme, context);
}

/// Write the shell's settings, recording the kit theme that is active now.
pub(crate) fn persist(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    if context.has_global::<Theme>() {
        shell.settings.theme_name = Theme::global(context).theme_name().to_string();
    }
    let settings = shell.settings.clone();
    let outcome = shell
        .settings_watcher
        .as_mut()
        .map(|watcher| watcher.write(&settings));
    if let Some(Err(refusal)) = outcome {
        refuse(shell, &refusal, context);
    }
}

/// Show a settings refusal without changing the values already in use.
pub(crate) fn refuse(
    shell: &mut WindowShell,
    refusal: &SettingsError,
    context: &mut Context<'_, WindowShell>,
) {
    shell.last_failure = Some(Notice {
        host: HostId("settings".to_owned()),
        kind: NoticeKind::Failure,
        detail: format!("{}: {}", refusal.field, refusal.message),
    });
    context.notify();
}
