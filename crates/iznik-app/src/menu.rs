//! The application's main menu: what the system menu bar shows while an
//! iznik window is frontmost.
//!
//! GPUI installs no main menu for an application that never asks for one, and
//! an application without a main menu shows the menu bar of whatever
//! application was frontmost before it — which is why an iznik window appeared
//! to carry another application's File, Edit and View menus until it was
//! clicked.
//!
//! [`install`] builds the bar once at startup. Application-wide items — about,
//! quit, hide, the source page — run from handlers registered here. Items that
//! need the window's own state are declared here as actions and handled by the
//! shell's own element tree, so an item is offered exactly when the focused
//! window can act on it.

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    App, Context, InteractiveElement, KeyBinding, Menu, MenuItem, NoAction, SystemMenuType, Window,
    actions,
};

use crate::actions::ActionId;
use crate::window::WindowShell;

/// The name shown in the application menu, the menu bar's first entry, and the
/// name every application-wide item carries.
pub const APPLICATION_NAME: &str = "iznik";

/// The page the Help menu opens.
const SOURCE_ADDRESS: &str = "https://github.com/koraytaylan/iznik";

actions!(
    iznik,
    [
        /// Show the application's name and version.
        About,
        /// Quit the application.
        Quit,
        /// Hide the application.
        Hide,
        /// Hide every other application.
        HideOthers,
        /// Show every application that was hidden.
        ShowAll,
        /// Open the project's page.
        ProjectPage,
        /// Create a session on the focused host.
        NewSession,
        /// Create a tab in the selected session.
        NewTab,
        /// Begin holding and connecting a host.
        AddHost,
        /// Close the selected tab.
        CloseTab,
        /// Close the focused pane.
        ClosePane,
        /// Open the settings window.
        OpenSettings,
        /// Show the command palette.
        CommandPalette,
        /// Put the terminal's selection on the clipboard.
        Copy,
        /// Send the clipboard's text to the focused pane.
        Paste,
        /// Minimize the front window.
        Minimize,
        /// Zoom the front window.
        Zoom,
    ]
);

/// Register every application-wide item's handler, bind the key equivalents
/// the menu advertises, and install the bar.
///
/// Called once, at application start, after the kit's own layers are
/// initialized. The inventory items are left to the shell's element tree,
/// which attaches them with [`attach`].
pub fn install(app: &mut App) {
    app.bind_keys(keybindings());
    app.on_action(|_: &About, app| about(app));
    app.on_action(|_: &Quit, app| app.quit());
    app.on_action(|_: &Hide, app| app.hide());
    app.on_action(|_: &HideOthers, app| app.hide_other_apps());
    app.on_action(|_: &ShowAll, app| app.unhide_other_apps());
    app.on_action(|_: &ProjectPage, app| app.open_url(SOURCE_ADDRESS));
    app.set_menus(menu_bar());
}

/// The key equivalents the menu advertises.
///
/// Copy and paste are scoped to the terminal's own key context, so a text
/// field's own copy and paste keep working; the rest are application-wide.
/// Tab and Shift-Tab are cleared in that same context. The window root
/// otherwise consumes them to move focus, and a shell never sees the key
/// it uses to complete a path.
fn keybindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-n", NewSession, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-shift-p", CommandPalette, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-c", Copy, Some("Terminal")),
        KeyBinding::new("cmd-v", Paste, Some("Terminal")),
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
    ]
}

/// Attach the shell's handlers for the menu items that act on its state to an
/// element in the window's own tree.
pub fn attach<Element>(element: Element, context: &mut Context<'_, WindowShell>) -> Element
where
    Element: InteractiveElement + 'static,
{
    element
        .on_action(context.listener(|shell, _: &NewSession, window, context| {
            run(shell, ActionId::CreateSession, window, context);
        }))
        .on_action(context.listener(|shell, _: &NewTab, window, context| {
            run(shell, ActionId::CreateTab, window, context);
        }))
        .on_action(context.listener(|shell, _: &AddHost, window, context| {
            run(shell, ActionId::AddHost, window, context);
        }))
        .on_action(context.listener(|shell, _: &CloseTab, window, context| {
            run(shell, ActionId::CloseTab, window, context);
        }))
        .on_action(context.listener(|shell, _: &ClosePane, window, context| {
            run(shell, ActionId::ClosePane, window, context);
        }))
        .on_action(
            context.listener(|shell, _: &OpenSettings, window, context| {
                run(shell, ActionId::OpenSettings, window, context);
            }),
        )
        .on_action(
            context.listener(|shell, _: &CommandPalette, _window, context| {
                shell.open_palette(context);
            }),
        )
        .on_action(|_: &Minimize, window, _app| window.minimize_window())
        .on_action(|_: &Zoom, window, _app| window.zoom_window())
}

/// Run one inventory action through the palette's own path: an argument-free
/// action is sent at once, one that needs an argument opens the palette at its
/// prompt, and one the model cannot support says so in a notice.
fn run(
    shell: &mut WindowShell,
    action: ActionId,
    window: &mut Window,
    context: &mut Context<'_, WindowShell>,
) {
    shell.choose(Some(action), window, context);
}

/// The complete menu bar, in display order.
#[must_use]
pub fn menu_bar() -> Vec<Menu> {
    vec![
        Menu::new(APPLICATION_NAME).items([
            MenuItem::action(format!("About {APPLICATION_NAME}"), About),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {APPLICATION_NAME}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APPLICATION_NAME}"), Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Session", NewSession),
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("Add Host\u{2026}", AddHost),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Pane", ClosePane),
            MenuItem::separator(),
            MenuItem::action("Settings\u{2026}", OpenSettings),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Copy", Copy),
            MenuItem::action("Paste", Paste),
        ]),
        Menu::new("View").items([MenuItem::action("Command Palette\u{2026}", CommandPalette)]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Help").items([MenuItem::action(
            format!("{APPLICATION_NAME} on GitHub"),
            ProjectPage,
        )]),
    ]
}

/// Show the application's name and version in a dialog over the front window.
fn about(app: &mut App) {
    let version = env!("CARGO_PKG_VERSION");
    let Some(window) = app.active_window() else {
        return;
    };
    let _opened = window.update(app, |_view, window, context| {
        window.open_alert_dialog(context, move |dialog, _, _| {
            dialog
                .title(format!("{APPLICATION_NAME} {version}"))
                .description("A terminal client that keeps sessions running on a host.")
        });
    });
}
