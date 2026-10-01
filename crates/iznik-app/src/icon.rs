//! The application's marks, taken from the published site.
//!
//! The medallion is the icon a dock, a taskbar, a window manager, the status
//! item and the about dialog show. The small tile — the site's favicon — is
//! only the smallest sizes of a desktop theme, where the medallion does not
//! stay legible. Bundles write the same bytes; this module is what a process
//! that was not launched from a bundle still shows.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::Arc;

use gpui_kit::{AnyWindowHandle, App, AssetSource, AsyncApp, Result, SharedString};
use image::RgbaImage;

use crate::menu::APPLICATION_NAME;

/// The medallion, as an embedded asset path.
pub const MARK: &str = "mark.svg";

/// The small tile, as an embedded asset path.
const SMALL_ICON: &str = "icon.svg";

/// The medallion rasterized for a window manager and the status item, 128
/// pixels on a side. The status item scales it down to the menu bar; the
/// pixels are the same picture the dock shows.
const WINDOW_ICON: &[u8] = include_bytes!("../assets/mark-128.png");

/// Menu id of the item that brings the windows forward.
const SHOW_ITEM: &str = "show";

/// Menu id of the item that closes the application.
const CLOSE_ITEM: &str = "close";

thread_local! {
    /// The status item, kept for the life of the process. Dropping the last
    /// handle removes it, so it stays here rather than in a frame.
    static APPLICATION_ICON: RefCell<Option<tray_icon::TrayIcon>> = const { RefCell::new(None) };
}

/// What a status-item menu asks the application to do.
#[derive(Clone, Copy)]
enum StatusAction {
    /// Bring the windows forward.
    Show,
    /// Close the application.
    Close,
}

/// The kit's icons, plus the two marks above.
///
/// The kit's source answers every other path, which is how its own icons
/// keep loading.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApplicationAssets;

impl AssetSource for ApplicationAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = asset_bytes(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        AssetSource::load(&gpui_kit::assets::Assets, path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        AssetSource::list(&gpui_kit::assets::Assets, path)
    }
}

/// The medallion as an RGBA image, for a window manager that asks the
/// process for one. macOS and Windows take the icon from the bundle instead.
///
/// `None` when the embedded image cannot be decoded.
#[must_use]
pub fn window_icon() -> Option<Arc<RgbaImage>> {
    let decoded = image::load_from_memory(WINDOW_ICON).ok()?;
    Some(Arc::new(decoded.into_rgba8()))
}

/// Install the status item: the menu bar on macOS, the notification area on
/// Windows, and the status notifier on Linux. It shows the same medallion as
/// the dock. A click brings the application's windows forward, and a right
/// click can show them or close the application. A session that has nowhere
/// to put one is logged, and the window still opens.
pub fn install(app: &App) {
    watch_click(app);
    install_icon();
}

/// The two marks, and nothing else.
fn asset_bytes(path: &str) -> Option<&'static [u8]> {
    match path {
        MARK => Some(include_bytes!("../assets/mark.svg")),
        SMALL_ICON => Some(include_bytes!("../assets/icon.svg")),
        _ => None,
    }
}

/// The medallion decoded for the status item.
fn status_icon() -> Option<tray_icon::Icon> {
    let decoded = image::load_from_memory(WINDOW_ICON).ok()?;
    let image = decoded.into_rgba8();
    let width = image.width();
    let height = image.height();
    tray_icon::Icon::from_rgba(image.into_raw(), width, height).ok()
}

/// Forward status-item clicks to the foreground, where the windows live.
///
/// The icon's own callback cannot touch the application: it has to be sent
/// across threads, and the application is not. The handler is registered
/// once for the process.
fn watch_click(app: &App) {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let menu_sender = sender.clone();
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
            if let Some(action) = action_for_item(event.id.as_ref()) {
                let _sent = menu_sender.send(action);
            }
        },
    ));
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event: tray_icon::TrayIconEvent| {
        if is_show_click(&event) {
            let _sent = sender.send(StatusAction::Show);
        }
    }));
    app.spawn(async move |application| {
        while let Some(action) = receiver.recv().await {
            apply(application, action);
        }
    })
    .detach();
}

/// The menu a right click opens. `None` when it cannot be built; a click
/// still brings the windows forward.
fn status_menu() -> Option<Box<dyn tray_icon::menu::ContextMenu>> {
    let menu = tray_icon::menu::Menu::new();
    let show = tray_icon::menu::MenuItem::with_id(
        SHOW_ITEM,
        format!("Show {APPLICATION_NAME}"),
        true,
        None,
    );
    let separator = tray_icon::menu::PredefinedMenuItem::separator();
    let close = tray_icon::menu::MenuItem::with_id(
        CLOSE_ITEM,
        format!("Quit {APPLICATION_NAME}"),
        true,
        None,
    );
    if menu.append(&show).is_err()
        || menu.append(&separator).is_err()
        || menu.append(&close).is_err()
    {
        return None;
    }
    Some(Box::new(menu))
}

/// Put the medallion in the status area.
fn install_icon() {
    let Some(icon) = status_icon() else {
        note_missing_icon();
        return;
    };
    match build_status_icon(icon) {
        Ok(installed) => keep_installed_icon(installed),
        Err(error) => note_failed_icon(&error),
    }
}

/// The embedded medallion could not be read as a status item.
fn note_missing_icon() {
    tracing::warn!("the status icon could not be read");
}

/// A status item carrying the medallion, with its menu when that menu can be
/// built.
///
/// # Errors
///
/// The status area's own error when the item cannot be created.
fn build_status_icon(icon: tray_icon::Icon) -> tray_icon::Result<tray_icon::TrayIcon> {
    let mut builder = tray_icon::TrayIconBuilder::new()
        .with_tooltip(APPLICATION_NAME)
        .with_icon(icon)
        .with_menu_on_left_click(false);
    if let Some(menu) = status_menu() {
        builder = builder.with_menu(menu);
    }
    builder.build()
}

/// Keep a status item for the life of the process.
fn keep_installed_icon(installed: tray_icon::TrayIcon) {
    APPLICATION_ICON.with(|held| {
        *held.borrow_mut() = Some(installed);
    });
}

/// The status area refused the item.
fn note_failed_icon(error: &tray_icon::Error) {
    tracing::warn!("the status icon was not installed: {error}");
}

/// The menu item's action, when it is one of ours.
fn action_for_item(item: &str) -> Option<StatusAction> {
    match item {
        SHOW_ITEM => Some(StatusAction::Show),
        CLOSE_ITEM => Some(StatusAction::Close),
        _ => None,
    }
}

/// A released left click. Press and release both report a click, and acting
/// on both would bring the windows forward twice.
fn is_show_click(event: &tray_icon::TrayIconEvent) -> bool {
    matches!(
        event,
        tray_icon::TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        }
    )
}

/// Run one status action against the open application.
fn apply(app: &mut AsyncApp, action: StatusAction) {
    match action {
        StatusAction::Show => present(app),
        StatusAction::Close => app.update(|application| application.quit()),
    }
}

/// Bring every window forward, the one that was already active last so it
/// stays the one in front.
fn present(app: &mut AsyncApp) {
    app.update(|application| {
        application.activate(true);
        let active = application.active_window();
        for window in application.windows() {
            if Some(window) != active {
                show_window(application, window);
            }
        }
        if let Some(window) = active {
            show_window(application, window);
        }
    });
}

/// Ask one window to take the foreground. A window that has already closed
/// is skipped.
fn show_window(application: &mut App, window: AnyWindowHandle) {
    let _shown = window.update(application, |_view, window, _application| {
        window.activate_window();
    });
}
