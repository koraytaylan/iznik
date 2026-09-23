//! Validated application settings, stored as one file and applied while running.
//!
//! The file lives at `$XDG_CONFIG_HOME/iznik/settings`, or
//! `~/.config/iznik/settings` when that variable is unset, or
//! `%APPDATA%\iznik\settings` on Windows when neither home is set. A missing
//! file is the built-in default. A malformed file is refused once per change,
//! with the previous values kept and the field named. A line this version
//! does not know — a newer setting, a comment — is ignored, reported once,
//! and kept when the application writes the file.

use gpui_kit::Context;
use gpui_kit::component::Theme;
use iznik_client::host::identity::HostId;
use libghostty_vt::style::RgbColor;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::actions::INVENTORY;
use crate::host_ui::{Notice, NoticeKind};
use crate::theme::AppTheme;
use crate::vt::{MAXIMUM_SCROLLBACK_BYTES, MINIMUM_SCROLLBACK_BYTES, SCROLLBACK_BYTES};
use crate::window::WindowShell;

/// The file name of the settings.
const SETTINGS_FILE: &str = "settings";

/// Smallest font size a setting may ask for, in logical pixels.
const MINIMUM_FONT_SIZE: f32 = 4.0;
/// Largest font size a setting may ask for, in logical pixels.
const MAXIMUM_FONT_SIZE: f32 = 200.0;
/// Smallest row height a setting may ask for, as a multiple of the font size.
const MINIMUM_LINE_HEIGHT: f32 = 0.5;
/// Largest row height a setting may ask for, as a multiple of the font size.
const MAXIMUM_LINE_HEIGHT: f32 = 4.0;
/// Every field this version reads and writes; any other line is kept as it is.
const OWNED_FIELDS: &[&str] = &[
    "foreground",
    "background",
    "font_family",
    "font_size",
    "line_height",
    "tabs_in_title_bar",
    "theme_name",
    "scrollback_bytes",
    "clipboard_write",
    "confirm_multiline_paste",
    "option_as_meta",
];
/// The prefix of a keybinding override's field.
const KEYBINDING_PREFIX: &str = "keybinding.";

/// Settings held by the application after validation.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Shared terminal and GPUI theme.
    pub theme: AppTheme,
    /// Name of the kit theme last chosen. Empty leaves the built-in default.
    pub theme_name: String,
    /// Keybinding overrides keyed by action name.
    pub keybindings: BTreeMap<String, String>,
    /// Per-pane emulator history budget in bytes, read when the application
    /// starts; clamped between the emulator's minimum and maximum.
    pub scrollback_bytes: usize,
    /// Whether a program in the focused pane may write the system clipboard
    /// with OSC 52.
    pub clipboard_write: bool,
    /// Whether a paste with line breaks into a program without bracketed
    /// paste waits for a person to confirm it.
    pub confirm_multiline_paste: bool,
    /// Whether Option is sent as Meta rather than used by the keyboard
    /// layout to type characters such as `@` on Turkish Q.
    pub option_as_meta: bool,
    /// Lines of the file this version does not own — unknown fields, comments
    /// and blank lines — written back as they were.
    pub unowned: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: AppTheme::default(),
            theme_name: String::new(),
            keybindings: BTreeMap::new(),
            scrollback_bytes: SCROLLBACK_BYTES,
            clipboard_write: true,
            confirm_multiline_paste: true,
            option_as_meta: false,
            unowned: Vec::new(),
        }
    }
}

/// Read the settings at `path`; a missing file is the built-in default.
///
/// # Errors
///
/// Returns a file error when the file cannot be read, or the field that
/// failed to decode.
pub fn read(path: &Path) -> Result<Settings, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => decode(&text),
        Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(io_error) => Err(error("file", &io_error.to_string())),
    }
}

/// The file this machine keeps: `$XDG_CONFIG_HOME/iznik/settings`, or
/// `~/.config/iznik/settings` when that variable is unset, or
/// `%APPDATA%\iznik\settings` on Windows when neither home is set.
///
/// `None` when neither home is known, which is when there is nowhere to write.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    crate::configuration_file::default_path(SETTINGS_FILE)
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
    /// Modification stamp last read, applied or refused.
    stamp: Option<SystemTime>,
    /// Unknown fields already reported, so each set is reported once.
    reported: Vec<String>,
}

impl Watcher {
    /// Create a watcher that has not applied any file yet.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stamp: None,
            reported: Vec::new(),
        }
    }

    /// Reload a changed file, returning whether a new settings value was applied.
    ///
    /// The stamp advances before the file is decoded, so a file that is
    /// refused is refused once, and read again only when it changes.
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
        self.stamp = Some(modified);
        let candidate = decode(
            &std::fs::read_to_string(&self.path)
                .map_err(|io_error| error("file", &io_error.to_string()))?,
        )?;
        replace(current, candidate)?;
        Ok(true)
    }

    /// The unknown fields of `settings`, when they are not the ones already
    /// reported; `None` once they have been.
    pub fn unreported(&mut self, settings: &Settings) -> Option<SettingsError> {
        let unknown = settings.unknown_fields();
        if unknown.is_empty() || unknown == self.reported {
            return None;
        }
        self.reported.clone_from(&unknown);
        Some(error(
            &unknown.join(", "),
            "unknown setting, ignored and kept in the file",
        ))
    }

    /// Replace the settings file and remember its modification stamp.
    ///
    /// Lines of the file on disk that this version does not own are kept,
    /// including ones added since it was read. The stamp is the one just
    /// written, so the next poll does not apply the same change a second time.
    ///
    /// # Errors
    ///
    /// Returns a file error when the directory cannot be created or the file
    /// cannot be replaced.
    pub fn write(&mut self, settings: &Settings) -> Result<(), SettingsError> {
        let mut merged = settings.clone();
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            merged.unowned = unowned_lines(&text);
        }
        write(&self.path, &merged)?;
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
    crate::configuration_file::replace(path, encode(settings).as_bytes())
        .map_err(|io_error| error("file", &io_error.to_string()))
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
        "foreground={},{},{}\nbackground={},{},{}\nfont_family={}\nfont_size={}\nline_height={}\ntabs_in_title_bar={}\ntheme_name={}\nscrollback_bytes={}\nclipboard_write={}\nconfirm_multiline_paste={}\noption_as_meta={}\n",
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
        settings.theme_name,
        settings.scrollback_bytes,
        settings.clipboard_write,
        settings.confirm_multiline_paste,
        settings.option_as_meta
    );
    for (action, chord) in &settings.keybindings {
        let _written = writeln!(text, "{KEYBINDING_PREFIX}{action}={chord}");
    }
    for line in &settings.unowned {
        let _written = writeln!(text, "{line}");
    }
    text
}

/// Whether `line` holds a field this version reads and writes.
fn owned(line: &str) -> bool {
    line.split_once('=').is_some_and(|(field, _value)| {
        OWNED_FIELDS.contains(&field) || field.starts_with(KEYBINDING_PREFIX)
    })
}

/// Every line of `text` this version does not own, in order.
fn unowned_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !owned(line))
        .map(str::to_owned)
        .collect()
}

impl Settings {
    /// Fields in the file that this version does not know.
    #[must_use]
    pub fn unknown_fields(&self) -> Vec<String> {
        self.unowned
            .iter()
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(field, _value)| field.to_owned())
            .collect()
    }
}

/// A number the field accepts: finite, and clamped to `range`.
///
/// # Errors
///
/// Returns a field-specific error when the value is not a finite number.
fn bounded(field: &str, value: &str, range: (f32, f32)) -> Result<f32, SettingsError> {
    let number: f32 = value
        .parse()
        .map_err(|_parse_error| error(field, "not a number"))?;
    if !number.is_finite() {
        return Err(error(field, "not a finite number"));
    }
    Ok(number.clamp(range.0, range.1))
}

/// `theme` with its font size and row height inside the ranges the grid can
/// draw; a value that is not finite becomes the default.
#[must_use]
pub fn drawable(theme: &AppTheme) -> AppTheme {
    let fallback = AppTheme::default();
    let fit = |value: f32, default: f32, range: (f32, f32)| {
        if value.is_finite() {
            value.clamp(range.0, range.1)
        } else {
            default
        }
    };
    AppTheme {
        font_size: fit(
            theme.font_size,
            fallback.font_size,
            (MINIMUM_FONT_SIZE, MAXIMUM_FONT_SIZE),
        ),
        line_height: fit(
            theme.line_height,
            fallback.line_height,
            (MINIMUM_LINE_HEIGHT, MAXIMUM_LINE_HEIGHT),
        ),
        ..theme.clone()
    }
}

/// Parse settings while naming the malformed field and preserving no partial result.
/// A comment, a blank line or a field this version does not know is kept
/// in [`Settings::unowned`] rather than refused.
///
/// # Errors
///
/// Returns a field-specific error for malformed colors, font size, or lines.
pub fn decode(text: &str) -> Result<Settings, SettingsError> {
    let mut settings = Settings::default();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            settings.unowned.push(line.to_owned());
            continue;
        }
        let Some((field, value)) = line.split_once('=') else {
            return Err(error("file", "line has no equals sign"));
        };
        match field {
            "foreground" => settings.theme.foreground = color(field, value)?,
            "background" => settings.theme.background = color(field, value)?,
            "font_family" => value.clone_into(&mut settings.theme.font_family),
            "theme_name" => value.clone_into(&mut settings.theme_name),
            "tabs_in_title_bar" => settings.theme.tabs_in_title_bar = flag(field, value)?,
            "line_height" => {
                settings.theme.line_height =
                    bounded(field, value, (MINIMUM_LINE_HEIGHT, MAXIMUM_LINE_HEIGHT))?;
            }
            "clipboard_write" => settings.clipboard_write = flag(field, value)?,
            "confirm_multiline_paste" => settings.confirm_multiline_paste = flag(field, value)?,
            "option_as_meta" => settings.option_as_meta = flag(field, value)?,
            "scrollback_bytes" => {
                settings.scrollback_bytes = value
                    .parse::<usize>()
                    .map_err(|_parse_error| error(field, "not a whole number of bytes"))?
                    .clamp(MINIMUM_SCROLLBACK_BYTES, MAXIMUM_SCROLLBACK_BYTES);
            }
            "font_size" => {
                settings.theme.font_size =
                    bounded(field, value, (MINIMUM_FONT_SIZE, MAXIMUM_FONT_SIZE))?;
            }
            key if key.starts_with(KEYBINDING_PREFIX) => {
                let action = key.trim_start_matches(KEYBINDING_PREFIX);
                if action.is_empty() {
                    return Err(error(field, "action name is empty"));
                }
                settings
                    .keybindings
                    .insert(action.to_owned(), value.to_owned());
            }
            _ => settings.unowned.push(line.to_owned()),
        }
    }
    Ok(settings)
}

/// Parse one `true` or `false` value.
///
/// # Errors
///
/// Returns a field-specific error for anything else.
fn flag(field: &str, value: &str) -> Result<bool, SettingsError> {
    value
        .parse()
        .map_err(|_parse_error| error(field, "not true or false"))
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
        self.preferences.settings()
    }

    /// Change the terminal behavior settings — the program clipboard, the
    /// multi-line paste question and Option as Meta — apply them to every
    /// pane and write the file.
    pub fn edit_behavior(
        &mut self,
        edit: impl FnOnce(&mut Behavior),
        context: &mut Context<'_, WindowShell>,
    ) {
        let settings = self.preferences.settings_mut();
        let mut behavior = Behavior {
            clipboard_write: settings.clipboard_write,
            confirm_multiline_paste: settings.confirm_multiline_paste,
            option_as_meta: settings.option_as_meta,
        };
        edit(&mut behavior);
        settings.clipboard_write = behavior.clipboard_write;
        settings.confirm_multiline_paste = behavior.confirm_multiline_paste;
        settings.option_as_meta = behavior.option_as_meta;
        let theme = settings.theme.clone();
        self.apply_theme(&theme, context);
        persist(self, context);
    }
}

/// The settings that change how a pane behaves rather than how it looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Behavior {
    /// Whether a program in the focused pane may write the system clipboard.
    pub clipboard_write: bool,
    /// Whether a multi-line paste into a program without bracketed paste waits
    /// for a person to confirm it.
    pub confirm_multiline_paste: bool,
    /// Whether Option is sent as Meta.
    pub option_as_meta: bool,
}

/// The settings a window runs with, the file they are kept in, and when that
/// file was last looked at.
///
/// Owned by the window as one piece, so that what reads, writes and polls the
/// file is here rather than spread over the window's own fields.
#[derive(Debug)]
pub(crate) struct Preferences {
    /// Validated settings in use.
    settings: Settings,
    /// The settings file, when the window was given one.
    watcher: Option<Watcher>,
    /// When the file was last looked at; `None` before the first look.
    polled: Option<Instant>,
    /// The system's font families, listed once: listing them walks every
    /// installed font, which is too slow to repeat for each theme change.
    installed_fonts: Option<Vec<String>>,
}

impl Preferences {
    /// The built-in settings, kept in the file at `path` when there is one.
    pub(crate) fn new(path: Option<PathBuf>) -> Preferences {
        Preferences {
            settings: Settings::default(),
            watcher: path.map(Watcher::new),
            polled: None,
            installed_fonts: None,
        }
    }

    /// The settings in use.
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The settings in use, to change; [`Preferences::write`] keeps a change.
    pub(crate) fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Read the file if it changed since it was last read, taking what it
    /// holds only when the whole of it is valid. `None` when there is no file.
    pub(crate) fn load(&mut self) -> Option<Result<bool, SettingsError>> {
        let watcher = self.watcher.as_mut()?;
        let mut settings = self.settings.clone();
        let outcome = watcher.reload(&mut settings);
        if matches!(outcome, Ok(true)) {
            self.settings = settings;
        }
        Some(outcome)
    }

    /// The same, at most once per `interval`: `None` when the last look was
    /// more recent, or there is no file.
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        interval: Duration,
    ) -> Option<Result<bool, SettingsError>> {
        if self
            .polled
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return None;
        }
        self.polled = Some(now);
        self.load()
    }

    /// The file's unknown fields, when they have not been reported yet.
    pub(crate) fn unreported(&mut self) -> Option<SettingsError> {
        let settings = &self.settings;
        self.watcher
            .as_mut()
            .and_then(|watcher| watcher.unreported(settings))
    }

    /// Write the settings in use to the file; `None` when there is no file.
    pub(crate) fn write(&mut self) -> Option<Result<(), SettingsError>> {
        let settings = &self.settings;
        self.watcher.as_mut().map(|watcher| watcher.write(settings))
    }

    /// The system's font families, listed by `list` the first time only.
    pub(crate) fn installed_fonts(&mut self, list: impl FnOnce() -> Vec<String>) -> &[String] {
        self.installed_fonts.get_or_insert_with(list)
    }
}

/// Read a saved file into the shell, then apply it. A missing file leaves the
/// built-in default in place and is not an error.
pub(crate) fn load_into(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    match shell.preferences.load() {
        Some(Ok(true)) => report_unknown(shell, context),
        Some(Err(refusal)) => refuse(shell, &refusal, context),
        Some(Ok(false)) | None => {}
    }
    apply_saved(shell, context);
}

/// Look at the settings file, at most once per `interval`, and apply what
/// changed in it.
pub(crate) fn poll(
    shell: &mut WindowShell,
    interval: Duration,
    context: &mut Context<'_, WindowShell>,
) {
    match shell.preferences.poll(Instant::now(), interval) {
        Some(Ok(true)) => {
            report_unknown(shell, context);
            apply_saved(shell, context);
        }
        Some(Err(refusal)) => refuse(shell, &refusal, context),
        Some(Ok(false)) | None => {}
    }
}

/// Report the file's unknown fields, once for each set of them.
pub(crate) fn report_unknown(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    if let Some(warning) = shell.preferences.unreported() {
        refuse(shell, &warning, context);
    }
}

/// Apply the shell's saved theme name and typography to every surface.
pub(crate) fn apply_saved(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    let settings = shell.preferences.settings();
    if !settings.theme_name.is_empty() && !crate::theme::apply_named(&settings.theme_name, context)
    {
        refuse(
            shell,
            &error("theme_name", "theme is not registered"),
            context,
        );
    }
    let theme = shell.preferences.settings().theme.clone();
    shell.apply_theme(&theme, context);
}

/// Write the shell's settings, recording the kit theme that is active now.
pub(crate) fn persist(shell: &mut WindowShell, context: &mut Context<'_, WindowShell>) {
    if context.has_global::<Theme>() {
        shell.preferences.settings_mut().theme_name =
            Theme::global(context).theme_name().to_string();
    }
    if let Some(Err(refusal)) = shell.preferences.write() {
        refuse(shell, &refusal, context);
    }
}

/// Show a settings refusal without changing the values already in use.
pub(crate) fn refuse(
    shell: &mut WindowShell,
    refusal: &SettingsError,
    context: &mut Context<'_, WindowShell>,
) {
    let _shown = shell.notices.fail(Notice {
        host: HostId("settings".to_owned()),
        kind: NoticeKind::Failure,
        detail: format!("{}: {}", refusal.field, refusal.message),
    });
    context.notify();
}
