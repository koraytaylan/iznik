//! The main window's size survives a relaunch, in logical pixels, on whichever
//! display is still connected.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{DisplayId, WindowBounds, bounds, point, px, size};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_app::window_bounds::{self, Screen};

#[path = "support/engine.rs"]
mod engine;

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// Width the shell is resized to, distinct from the test display.
const RESIZED_WIDTH: f32 = 640.0;
/// Height the shell is resized to.
const RESIZED_HEIGHT: f32 = 480.0;

/// A bad line is skipped and the record that parses is kept.
///
/// # Panics
///
/// Panics when the reported window is too small to record, or the text does not decode to it.
#[test]
fn a_record_round_trips_and_skips_a_bad_line() {
    let reported = WindowBounds::Maximized(bounds(
        point(px(-40.0), px(80.0)),
        size(px(1280.0), px(800.0)),
    ));
    let record = window_bounds::capture(reported, Some("display-a".to_owned()))
        .expect("the reported window is large enough");
    let text = format!("not a record\n{}", window_bounds::encode(&record));
    assert_eq!(
        window_bounds::decode(&text),
        Some(record),
        "the first line that parses is the record"
    );
}

/// A display that is still connected keeps the position the platform reported.
///
/// # Panics
///
/// Panics when the fixture record does not parse, or the placement moves it.
#[test]
fn a_known_display_keeps_the_position_the_platform_reported() {
    let screens = vec![
        screen(1, "primary", 0.0, 0.0, 1_440.0, 900.0),
        screen(2, "secondary", 2_000.0, 0.0, 1_000.0, 800.0),
    ];
    let record = window_bounds::decode("windowed secondary 40 50 800 600\n")
        .expect("the fixture record parses");
    let placed = window_bounds::place(&record, &screens);
    assert_eq!(
        placed.bounds,
        WindowBounds::Windowed(bounds(
            point(px(40.0), px(50.0)),
            size(px(800.0), px(600.0))
        )),
        "a position the platform reported is handed back unchanged"
    );
    assert_eq!(
        placed.display,
        Some(DisplayId::new(2)),
        "the window opens on the display it was on"
    );
}

/// A position on no connected display is centered, and stays maximized.
///
/// # Panics
///
/// Panics when the fixture record does not parse, or the placement is not centered.
#[test]
fn a_position_on_no_display_is_centered_at_the_recorded_size() {
    let screens = vec![screen(1, "primary", 0.0, 0.0, 1_000.0, 800.0)];
    let record = window_bounds::decode("maximized gone 8000 8000 800 600\n")
        .expect("the fixture record parses");
    let placed = window_bounds::place(&record, &screens);
    assert_eq!(
        placed.bounds,
        WindowBounds::Maximized(bounds(
            point(px(100.0), px(100.0)),
            size(px(800.0), px(600.0))
        )),
        "a lost display centers the recorded size, and stays maximized"
    );
    assert_eq!(placed.display, Some(DisplayId::new(1)));
}

/// A record larger than the only display is fitted to that display.
///
/// # Panics
///
/// Panics when the fixture record does not parse, or the placement is not fitted.
#[test]
fn a_record_larger_than_the_display_is_fitted_to_it() {
    let screens = vec![screen(1, "primary", 0.0, 0.0, 1_000.0, 800.0)];
    let record = window_bounds::decode("fullscreen 9000 9000 2000 2000\n")
        .expect("the fixture record parses");
    let placed = window_bounds::place(&record, &screens);
    assert_eq!(
        placed.bounds,
        WindowBounds::Fullscreen(bounds(
            point(px(0.0), px(0.0)),
            size(px(1_000.0), px(800.0)),
        )),
        "a window larger than the display opens fitted to it, still fullscreen"
    );
}

#[gpui_kit::test]
fn a_resized_window_is_what_the_next_launch_reads(context: &mut gpui_kit::TestAppContext) {
    check(&resized(context));
}

/// Keep the assertion outside the GPUI test macro's generated documentation.
///
/// # Panics
///
/// Fails on a fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Resize a shell and read the record back the way the next launch does.
///
/// # Errors
///
/// Returns a fixture or window failure.
///
/// # Panics
///
/// Panics when the next launch would not open at the resized size.
fn resized(context: &mut gpui_kit::TestAppContext) -> Result<(), Failed> {
    a_record_round_trips_and_skips_a_bad_line();
    a_known_display_keeps_the_position_the_platform_reported();
    a_position_on_no_display_is_centered_at_the_recorded_size();
    a_record_larger_than_the_display_is_fitted_to_it();
    context.update(gpui_kit::init);
    let (bridge, _directory) = engine::start("window-bounds")?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let scratch = Scratch::new("window-bounds")?;
    let path = scratch.0.join("window-bounds");
    let handle = context.add_window(|window, build| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                bounds_path: Some(path.clone()),
                bounds_write_delay: Duration::ZERO,
                ..ShellOptions::default()
            },
            window,
            build,
        )
    });
    context.simulate_window_resize(handle.into(), size(px(RESIZED_WIDTH), px(RESIZED_HEIGHT)));
    let placed = context
        .update(|app| window_bounds::saved(&path, app))
        .ok_or("the resized window was written")?;
    assert_eq!(
        placed.bounds.get_bounds().size,
        size(px(RESIZED_WIDTH), px(RESIZED_HEIGHT)),
        "the next launch reads the size the window was resized to"
    );
    Ok(())
}

/// One display in a placement fixture.
fn screen(id: u64, token: &str, horizontal: f32, vertical: f32, width: f32, height: f32) -> Screen {
    Screen {
        id: DisplayId::new(id),
        token: Some(token.to_owned()),
        bounds: bounds(
            point(px(horizontal), px(vertical)),
            size(px(width), px(height)),
        ),
    }
}

/// A scratch directory removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    /// Create a scratch directory.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the directory cannot be created.
    fn new(label: &str) -> Result<Self, Failed> {
        let path = iznik_testkit::scratch::path(label);
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}
