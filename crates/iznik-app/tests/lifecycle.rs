//! Cross-window application lifecycle: closing the main window ends the app.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{AppContext as _, TestAppContext};
use iznik_app::EmptyView;
use iznik_app::lifecycle::on_window_closed;

/// Errors propagated by the GPUI fixture.
type Failed = Box<dyn std::error::Error>;

#[gpui_kit::test]
fn only_the_named_window_closing_calls_the_callback(context: &mut TestAppContext) {
    check(&closes_only_for_the_named_window(context));
}

/// A registered callback distinguishes the window it names from any other
/// window closing, the way the binary distinguishes its main window from a
/// secondary settings window so closing settings alone never quits the app.
///
/// # Errors
/// Propagates fixture and window-update failures.
///
/// # Panics
/// Fails if the callback is called for the wrong window, or is not called
/// for the named one.
fn closes_only_for_the_named_window(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let main = context.add_window(|_window, _build_context| EmptyView);
    let other = context.add_window(|_window, _build_context| EmptyView);
    let called = Rc::new(Cell::new(false));
    let observed = Rc::clone(&called);
    context.update(|app| {
        on_window_closed(app, main.window_id(), move |_app| observed.set(true)).detach();
    });

    context.update_window(other.into(), |_view, window, _app| {
        window.remove_window();
    })?;
    assert!(
        !called.get(),
        "closing a window other than the named one does not call the callback"
    );

    context.update_window(main.into(), |_view, window, _app| {
        window.remove_window();
    })?;
    assert!(called.get(), "closing the named window calls the callback");
    Ok(())
}

/// Keep fixture assertion outside the GPUI macro's generated test documentation.
///
/// # Panics
/// Fails on a fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}
