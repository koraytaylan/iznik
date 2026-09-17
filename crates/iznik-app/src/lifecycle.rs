//! Cross-window application lifecycle glue.

use gpui_kit::{App, Subscription, WindowId};

/// Register a callback that fires once the given window closes, ignoring
/// every other window's close. Lets the caller end the application only
/// when its *main* window goes away, not a secondary window like the
/// settings panel — `App::on_window_closed` alone reports every window's
/// close identically, with no way to tell them apart but the id.
pub fn on_window_closed(
    app_context: &App,
    window: WindowId,
    mut on_closed: impl FnMut(&mut App) + 'static,
) -> Subscription {
    app_context.on_window_closed(move |app, closed_window| {
        if closed_window == window {
            on_closed(app);
        }
    })
}
