//! A focused pane's block cursor fills its cell and redraws the character in
//! the cell's background, and its bar is the bar the program asked for.

mod support;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, TestAppContext};
use iznik_app::grid::{GridMetrics, TerminalGrid, draw_list};
use iznik_app::vt::{VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;
use support::{key, open, snapshot};

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;
/// The cursor color the fixture's program sets.
const CURSOR_COLOR: u32 = 0x00a1_b2c3;

/// Report fixture failures outside the GPUI macro.
///
/// # Panics
/// Fails with the fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// The block cursor's cell is its own run, filled with the cursor color and
/// drawn in the cell's background.
///
/// # Panics
/// Panics when the cell under the block is not recolored.
#[test]
fn focused_block_draws_its_character_in_the_background() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 6, 1).expect("open");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(0),
            bytes: b"\x1b]12;rgb:a1/b2/c3\x1b\\\x1b[2 qabc\x1b[2D".to_vec(),
            receipt: None,
        })
        .expect("feed");
    let frame = snapshot(&thread).expect("snapshot");
    let rows = draw_list(&frame, None).expect("draw list");
    let under = rows[0]
        .runs
        .iter()
        .find(|run| run.column == 1)
        .expect("a run starts at the cursor");
    assert_eq!(under.text, "b", "the cell under the cursor is its own run");
    assert_eq!(under.background, frame.colors.cursor.expect("cursor color"));
    assert_eq!(
        under.foreground, frame.colors.background,
        "drawn in the background"
    );
}

/// A focused pane draws the bar cursor its program asked for.
#[gpui_kit::test]
fn focused_bar_is_a_bar(context: &mut TestAppContext) {
    check(&bar(context));
}

/// Paint a focused grid and find its one-pixel bar.
///
/// # Errors
/// Returns terminal, grid or window failures.
///
/// # Panics
/// Panics when the cursor is not a bar.
fn bar(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    open(&thread, Sequence(0), 4, 1)?;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"\x1b]12;rgb:a1/b2/c3\x1b\\\x1b[6 qA".to_vec(),
    })?;
    let current = snapshot(&thread)?;
    handle.update(context, |grid, _window, context| {
        grid.set_focused(true, context)?;
        grid.apply(current, context)
    })??;
    context.update_window(handle.into(), |_view, window, application| {
        window.draw(application).clear(application);
    })?;
    handle.update(context, |_grid, window, _application| {
        let origin = window.find("terminal-grid").bounds().origin;
        let scale = window.scale_factor();
        let tint: gpui_kit::Hsla = gpui_kit::rgb(CURSOR_COLOR).into();
        let painted: Vec<(f32, f32, f32, f32)> = window
            .painted_quads()
            .into_iter()
            .filter(|rectangle| rectangle.background.as_solid() == Some(tint))
            .map(|rectangle| {
                (
                    rectangle.bounds.origin.x.0 / scale - f32::from(origin.x),
                    rectangle.bounds.origin.y.0 / scale - f32::from(origin.y),
                    rectangle.bounds.size.width.0 / scale,
                    rectangle.bounds.size.height.0 / scale,
                )
            })
            .collect();
        assert_eq!(painted, [(8.0, 0.0, 1.0, 18.0)], "a one-pixel bar");
    })?;
    Ok(())
}
