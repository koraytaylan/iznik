//! The headless smoke: a GPUI window drawn and asserted without a display.
//!
//! GPUI's test context builds a real window that this process renders itself,
//! so this test is what proves the crate's suite runs headlessly under
//! nextest on the Linux gate machine. The window is the crate's own themed
//! empty view, rendered through the same `Render` implementation the binary
//! shows.

use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, TestAppContext};
use iznik_app::EmptyView;

#[gpui_kit::test]
fn headless_window_draws_its_element_tree(context: &mut TestAppContext) {
    context.update(gpui_kit::init);
    let handle = context.add_window(|_window, _build_context| EmptyView);
    assert_the_drawn_surface(context, handle.into());
}

/// Draws the window and asserts what its observed element tree reported.
///
/// # Panics
///
/// When the window cannot be updated, or the surface or its labeled element
/// do not report what the view was drawn with — the failing assertions this
/// suite exists to make. The reasons live here and not on the annotated
/// function above, because that annotation moves the enclosing function's
/// documentation onto its generated wrapper.
fn assert_the_drawn_surface(context: &mut TestAppContext, window_handle: AnyWindowHandle) {
    let updated = context.update_window(window_handle, |_view, window, app_context| {
        window.draw(app_context).clear(app_context);
        let surface = window.find("empty-view-surface");
        assert!(surface.visible(), "the window's surface is drawn");
        assert_eq!(
            surface.label(),
            Some("iznik"),
            "the surface carries the view's label"
        );
        assert!(
            window.try_find("empty-view-label").is_some(),
            "the labeled element of the view is in the tree"
        );
    });
    assert!(
        updated.is_ok(),
        "the window could not be updated: {updated:?}"
    );
}
