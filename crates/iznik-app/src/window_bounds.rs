//! The main window's size and place, written so the next launch opens there.
//!
//! GPUI reports the same [`WindowBounds`] on macOS, Windows and Linux: logical
//! pixels, and whether the window is ordinary, maximized or fullscreen. The
//! record stores that, plus the display's stable identifier when the platform
//! has one. The next launch passes it back. A display that is no longer
//! connected, or a position that sits on none of the connected displays, opens
//! the same size centered on the primary display.
//!
//! The file is one line, replaced atomically:
//! `windowed <display> <horizontal> <vertical> <width> <height>`. The display
//! token is omitted when the platform has none. Numbers are whole logical pixels.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::{
    App, Bounds, Context, DisplayId, Pixels, Point, Size, Subscription, Window, WindowBounds,
    bounds, point, px, size,
};

use crate::window::WindowShell;

/// The file name of the record, beside the settings file.
const FRAME_FILE: &str = "window-bounds";
/// How long a moved or resized window waits before the record is written.
pub(crate) const WRITE_DELAY: Duration = Duration::from_secs(1);
/// Smallest edge that is a real window, in logical pixels. Smaller is a
/// window that has not been laid out, and is not recorded.
const MINIMUM_EXTENT: i16 = 200;
/// Largest edge recorded, in logical pixels. Larger is a damaged record.
const MAXIMUM_EXTENT: i16 = 16_384;
/// Least origin recorded, in logical pixels. Further off is brought back in.
const MINIMUM_ORIGIN: i16 = -16_384;
/// Greatest origin recorded, in logical pixels.
const MAXIMUM_ORIGIN: i16 = 16_384;
/// Height of the top strip that must still meet a display, in logical pixels.
const TITLE_BAND: i16 = 24;
/// How much of that strip must meet a display before the position is kept.
const MINIMUM_VISIBLE: i16 = 48;
/// Splits a spare edge in two, so a centered window has equal space on each side.
const CENTER: f32 = 2.0;
/// The word for an ordinary window.
const WINDOWED: &str = "windowed";
/// The word for a maximized window. The numbers are its restore size.
const MAXIMIZED: &str = "maximized";
/// The word for a fullscreen window. The numbers are its restore size.
const FULLSCREEN: &str = "fullscreen";

/// Whether the window was ordinary, maximized or fullscreen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameState {
    /// An ordinary window at the recorded bounds.
    Windowed,
    /// Maximized. The bounds are the size it returns to.
    Maximized,
    /// Fullscreen. The bounds are the size it returns to.
    Fullscreen,
}

/// One recorded window: its state, the display it was on, and its bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRecord {
    /// Ordinary, maximized or fullscreen.
    state: FrameState,
    /// The display's stable identifier, when the platform reported one.
    display: Option<String>,
    /// Horizontal origin, in whole logical pixels.
    horizontal: i16,
    /// Vertical origin, in whole logical pixels.
    vertical: i16,
    /// Width, in whole logical pixels.
    width: i16,
    /// Height, in whole logical pixels.
    height: i16,
}

/// A connected display the recorded window may open on.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// Platform identifier for this launch. It is not stable across restarts.
    pub id: DisplayId,
    /// Stable identifier, compared with the record. `None` when the platform has none.
    pub token: Option<String>,
    /// Usable area, excluding a taskbar or dock.
    pub bounds: Bounds<Pixels>,
}

/// Where the next launch opens: the bounds GPUI is given, and which display.
#[derive(Clone, Debug, PartialEq)]
pub struct FramePlacement {
    /// Bounds, including maximized or fullscreen when that was recorded.
    pub bounds: WindowBounds,
    /// Display to open on, when the recorded one is still connected.
    pub display: Option<DisplayId>,
}

/// The size and place last seen, written after it settles.
#[derive(Debug, Default)]
pub(crate) struct FrameWatch {
    /// The frame last reported, when it was large enough to record.
    latest: Option<FrameRecord>,
    /// When that frame first differed from what the file holds.
    since: Option<Instant>,
    /// Keeps the bounds observer alive for the life of the shell.
    subscription: Option<Subscription>,
}

impl FrameWatch {
    /// A watch that has not seen a window yet.
    #[must_use]
    pub(crate) fn idle() -> Self {
        Self::default()
    }

    /// Remember `bounds` when they differ from the frame already held.
    ///
    /// `display` is the platform's stable identifier for the screen the window
    /// is on, when it has one.
    pub(crate) fn note(&mut self, reported: WindowBounds, display: Option<String>) -> bool {
        let Some(record) = capture(reported, display) else {
            return false;
        };
        if self.latest.as_ref() == Some(&record) {
            return false;
        }
        self.latest = Some(record);
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
        true
    }

    /// Keep `subscription` so the bounds observer is not dropped.
    pub(crate) fn hold(&mut self, subscription: Subscription) {
        self.subscription = Some(subscription);
    }
}

impl Drop for FrameWatch {
    fn drop(&mut self) {
        drop(self.subscription.take());
    }
}

/// The record this machine keeps: `$XDG_CONFIG_HOME/iznik/window-bounds`, or
/// `~/.config/iznik/window-bounds`, or `%APPDATA%\iznik\window-bounds` on
/// Windows when neither home is set.
///
/// `None` when no configuration home is known.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    crate::configuration_file::default_path(FRAME_FILE)
}

/// Read a record. A missing, unreadable or damaged file is `None`: the window
/// opens where the platform places a new one.
#[must_use]
pub fn load(path: &Path) -> Option<FrameRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    decode(&text)
}

/// Replace the record at `path`, creating its directory when it is missing.
///
/// # Errors
///
/// Returns the operating system's error when the directory cannot be created
/// or the file cannot be replaced. The previous record is left in place when
/// the temporary file cannot be written.
pub fn write(path: &Path, record: &FrameRecord) -> Result<(), std::io::Error> {
    crate::configuration_file::replace(path, encode(record).as_bytes())
}

/// The record as the file holds it.
#[must_use]
pub fn encode(record: &FrameRecord) -> String {
    let state = match record.state {
        FrameState::Windowed => WINDOWED,
        FrameState::Maximized => MAXIMIZED,
        FrameState::Fullscreen => FULLSCREEN,
    };
    match &record.display {
        Some(display) => format!(
            "{state} {display} {} {} {} {}\n",
            record.horizontal, record.vertical, record.width, record.height
        ),
        None => format!(
            "{state} {} {} {} {}\n",
            record.horizontal, record.vertical, record.width, record.height
        ),
    }
}

/// The record a file's text holds. A line that does not parse is skipped, and
/// the first line that does is the record.
#[must_use]
pub fn decode(text: &str) -> Option<FrameRecord> {
    text.lines().find_map(decode_line)
}

/// The frame GPUI reported, or `None` when its size is not a real window.
#[must_use]
pub fn capture(reported: WindowBounds, display: Option<String>) -> Option<FrameRecord> {
    let (state, reported) = match reported {
        WindowBounds::Windowed(reported) => (FrameState::Windowed, reported),
        WindowBounds::Maximized(reported) => (FrameState::Maximized, reported),
        WindowBounds::Fullscreen(reported) => (FrameState::Fullscreen, reported),
    };
    let horizontal = whole_origin(reported.origin.x)?;
    let vertical = whole_origin(reported.origin.y)?;
    let width = whole_extent(reported.size.width)?;
    let height = whole_extent(reported.size.height)?;
    let display = display.filter(|token| !token.is_empty() && token.parse::<i16>().is_err());
    Some(FrameRecord {
        state,
        display,
        horizontal,
        vertical,
        width,
        height,
    })
}

/// Where `record` opens among `screens`. The primary display is first.
///
/// A record whose display is still connected keeps the position the platform
/// reported, which is what that platform expects to be handed back. Anything
/// else that meets no connected display is centered on the first screen, at
/// the recorded size or that screen's size when the record is larger.
#[must_use]
pub fn place(record: &FrameRecord, screens: &[Screen]) -> FramePlacement {
    let (placed, display) = located(record, screens);
    FramePlacement {
        bounds: with_state(record.state, placed),
        display,
    }
}

/// The placement the file at `path` asks for on the displays `app` has now.
///
/// `None` when the file is missing or damaged.
#[must_use]
pub fn saved(path: &Path, app: &App) -> Option<FramePlacement> {
    Some(place(&load(path)?, &screens_of(app)))
}

/// Start recording `window` into the shell's bounds file, when it has one.
pub(crate) fn watch(
    shell: &mut WindowShell,
    window: &mut Window,
    context: &mut Context<'_, WindowShell>,
) {
    if shell.options.bounds_path.is_none() {
        return;
    }
    let _changed = shell
        .frame_watch
        .note(window.window_bounds(), display_token(window, context));
    shell.write_window_bounds(false);
    let subscription = context.observe_window_bounds(window, |observed, window, context| {
        if observed
            .frame_watch
            .note(window.window_bounds(), display_token(window, context))
        {
            observed.write_window_bounds(false);
        }
    });
    shell.frame_watch.hold(subscription);
}

impl WindowShell {
    /// Write the window's size and place, once it has waited the shell's
    /// bounds write delay, or when `now` is asked for.
    ///
    /// A record that cannot be written leaves the previous file in place.
    pub(crate) fn write_window_bounds(&mut self, now: bool) {
        let Some(since) = self.frame_watch.since else {
            return;
        };
        if !now && since.elapsed() < self.options.bounds_write_delay {
            return;
        }
        self.frame_watch.since = None;
        let Some(path) = self.options.bounds_path.clone() else {
            return;
        };
        let Some(record) = self.frame_watch.latest.clone() else {
            return;
        };
        let _ignored = write(&path, &record);
    }
}

/// One parsed line, or `None` when it is not a record.
fn decode_line(line: &str) -> Option<FrameRecord> {
    let mut parts = line.split_whitespace();
    let state = state_of(parts.next()?)?;
    let head = parts.next()?;
    let (display, horizontal) = if let Ok(horizontal) = head.parse::<i16>() {
        (None, horizontal)
    } else {
        (Some(head.to_owned()), parts.next()?.parse().ok()?)
    };
    let vertical = parts.next()?.parse().ok()?;
    let width = parts.next()?.parse().ok()?;
    let height = parts.next()?.parse().ok()?;
    if parts.next().is_some()
        || !extent_in_range(width)
        || !extent_in_range(height)
        || !origin_in_range(horizontal)
        || !origin_in_range(vertical)
    {
        return None;
    }
    Some(FrameRecord {
        state,
        display,
        horizontal,
        vertical,
        width,
        height,
    })
}

/// The state a record's first word names.
fn state_of(token: &str) -> Option<FrameState> {
    match token {
        WINDOWED => Some(FrameState::Windowed),
        MAXIMIZED => Some(FrameState::Maximized),
        FULLSCREEN => Some(FrameState::Fullscreen),
        _ => None,
    }
}

/// Whether `whole` is a size this record keeps.
fn extent_in_range(whole: i16) -> bool {
    (MINIMUM_EXTENT..=MAXIMUM_EXTENT).contains(&whole)
}

/// Whether `whole` is an origin this record keeps.
fn origin_in_range(whole: i16) -> bool {
    (MINIMUM_ORIGIN..=MAXIMUM_ORIGIN).contains(&whole)
}

/// The bounds `record` names, in logical pixels.
fn recorded_bounds(record: &FrameRecord) -> Bounds<Pixels> {
    bounds(
        point(from_whole(record.horizontal), from_whole(record.vertical)),
        size(from_whole(record.width), from_whole(record.height)),
    )
}

/// `bounds` wrapped in `state`.
fn with_state(state: FrameState, bounds: Bounds<Pixels>) -> WindowBounds {
    match state {
        FrameState::Windowed => WindowBounds::Windowed(bounds),
        FrameState::Maximized => WindowBounds::Maximized(bounds),
        FrameState::Fullscreen => WindowBounds::Fullscreen(bounds),
    }
}

/// The screen and bounds `record` opens at.
fn located(record: &FrameRecord, screens: &[Screen]) -> (Bounds<Pixels>, Option<DisplayId>) {
    let recorded = recorded_bounds(record);
    if let Some(screen) = matched(record, screens) {
        return (recorded, Some(screen.id));
    }
    if let Some(screen) = screens
        .iter()
        .find(|screen| title_visible(&screen.bounds, &recorded))
    {
        return (recorded, Some(screen.id));
    }
    let Some(screen) = screens.first() else {
        return (recorded, None);
    };
    let fitted = recorded.size.min(&screen.bounds.size);
    (
        bounds(center_origin(&screen.bounds, fitted), fitted),
        Some(screen.id),
    )
}

/// The screen whose stable identifier is the one `record` names.
fn matched<'screens>(
    record: &FrameRecord,
    screens: &'screens [Screen],
) -> Option<&'screens Screen> {
    let token = record.display.as_ref()?;
    screens
        .iter()
        .find(|screen| screen.token.as_ref() == Some(token))
}

/// Whether the top of `window` still meets `screen`, so it can be dragged back.
fn title_visible(screen: &Bounds<Pixels>, window: &Bounds<Pixels>) -> bool {
    let band = bounds(
        window.origin,
        size(window.size.width, from_whole(TITLE_BAND)),
    );
    let shared = screen.intersect(&band);
    shared.size.width >= from_whole(MINIMUM_VISIBLE) && shared.size.height > px(0.0)
}

/// The origin that centers `window` in `screen`.
fn center_origin(screen: &Bounds<Pixels>, window: Size<Pixels>) -> Point<Pixels> {
    point(
        offset(
            screen.origin.x,
            half(distance(screen.size.width, window.width)),
        ),
        offset(
            screen.origin.y,
            half(distance(screen.size.height, window.height)),
        ),
    )
}

/// The displays `app` has, primary first.
fn screens_of(app: &App) -> Vec<Screen> {
    let primary = app.primary_display().map(|display| display.id());
    let mut listed = app.displays();
    listed.sort_by_key(|display| primary != Some(display.id()));
    listed
        .into_iter()
        .map(|display| Screen {
            id: display.id(),
            token: display.uuid().ok().map(|identifier| identifier.to_string()),
            bounds: display.visible_bounds(),
        })
        .collect()
}

/// The stable display identifier, when the platform reports one.
fn display_token(window: &Window, app: &App) -> Option<String> {
    window
        .display(app)
        .and_then(|display| display.uuid().ok())
        .map(|identifier| identifier.to_string())
}

/// A whole logical-pixel extent, or `None` when it is not a real window.
fn whole_extent(value: Pixels) -> Option<i16> {
    let whole = f32::from(value).round();
    if !whole.is_finite() || whole < f32::from(MINIMUM_EXTENT) || whole > f32::from(MAXIMUM_EXTENT)
    {
        return None;
    }
    format!("{whole:.0}").parse().ok()
}

/// A whole logical-pixel origin. An origin past the recorded range is brought
/// in to that range; a value that is not a number is `None`.
fn whole_origin(value: Pixels) -> Option<i16> {
    let whole = f32::from(value).round();
    if !whole.is_finite() {
        return None;
    }
    if whole < f32::from(MINIMUM_ORIGIN) {
        return Some(MINIMUM_ORIGIN);
    }
    if whole > f32::from(MAXIMUM_ORIGIN) {
        return Some(MAXIMUM_ORIGIN);
    }
    format!("{whole:.0}").parse().ok()
}

/// `whole` as logical pixels.
fn from_whole(whole: i16) -> Pixels {
    px(f32::from(whole))
}

/// Half of `value`, for centering.
fn half(value: Pixels) -> Pixels {
    finite(f32::from(value) / CENTER)
}

/// Add two logical coordinates, keeping the result a finite pixel value.
fn offset(left: Pixels, right: Pixels) -> Pixels {
    finite(f32::from(left) + f32::from(right))
}

/// Subtract two logical coordinates, keeping the result a finite pixel value.
fn distance(left: Pixels, right: Pixels) -> Pixels {
    finite(f32::from(left) - f32::from(right))
}

/// `value` as pixels, or zero when it is not a finite number.
fn finite(value: f32) -> Pixels {
    px(if value.is_finite() { value } else { 0.0 })
}
