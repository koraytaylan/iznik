//! The application installs a main menu, so the system menu bar describes
//! iznik rather than whatever application was frontmost before it, and the
//! menu's terminal items act on the focused pane.

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{App, AppContext as _, ClipboardItem, Focusable, Subscription, TestAppContext};
use iznik_app::grid::{GridInput, GridMetrics, TerminalGrid};
use iznik_app::input::TerminalInput;
use iznik_app::menu;
use iznik_app::vt::{VtOptions, VtThread};
use iznik_protocol::identity::Sequence;

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;

/// Fixture width leaves room for a prompt and pasted text.
const COLUMNS: u16 = 40;
/// Fixture height includes a nonzero cursor row.
const ROWS: u16 = 4;
/// The text put on the clipboard and expected to reach the terminal.
const CLIPBOARD_TEXT: &str = "pasted text";

/// Installing the menu bar fills it with the application's own named entries.
#[gpui_kit::test]
fn the_application_installs_its_own_menu_bar(context: &mut TestAppContext) {
    check(&installs(context));
}

/// The menu's Paste item sends the clipboard's text to the focused pane
/// through the native encoder.
#[gpui_kit::test]
fn the_menu_paste_item_reaches_the_focused_pane(context: &mut TestAppContext) {
    check(&paste_reaches(context));
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Initialize the application, install the menu, and assert the bar is the
/// application's own rather than empty.
///
/// # Errors
/// Returns the assertion failure when a menu or item is missing.
fn installs(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| {
        gpui_kit::init(app);
        menu::install(app);
        let installed = app.get_menus().ok_or("the platform reports no menu bar")?;
        let names: Vec<&str> = installed.iter().map(|entry| entry.name.as_ref()).collect();
        for expected in [
            menu::APPLICATION_NAME,
            "File",
            "Edit",
            "View",
            "Window",
            "Help",
        ] {
            if !names.contains(&expected) {
                return Err(format!("the menu bar is missing `{expected}`: {names:?}").into());
            }
        }
        Ok(())
    })
}

/// Focus a real grid, put text on the clipboard, dispatch the menu's Paste
/// item, and assert the grid asked its encoder to paste exactly that text.
///
/// # Errors
/// Returns emulator, window or assertion failures.
///
/// # Panics
/// Fails when the thread misses a reply deadline.
fn paste_reaches(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let thread = VtThread::start(VtOptions::default())?;
    let frame = support::open(&thread, Sequence(0), COLUMNS, ROWS)?;
    let requests = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&requests);
    let mut subscription: Option<Subscription> = None;
    let handle =
        context.add_window(|_, context| TerminalGrid::new(GridMetrics::default(), context));
    handle.update(context, |grid, window, context| {
        grid.apply(frame, context)?;
        window.focus(&grid.focus_handle(context), context);
        subscription = Some(context.subscribe(
            &context.entity(),
            move |_, _, event: &GridInput, _| {
                observed.borrow_mut().push(event.clone());
            },
        ));
        Ok::<_, Failed>(())
    })??;
    context.update(|app: &mut App| {
        app.write_to_clipboard(ClipboardItem::new_string(CLIPBOARD_TEXT.to_owned()));
    });
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    context.dispatch_action(handle.into(), menu::Paste);
    let arrived = requests
        .borrow()
        .iter()
        .any(|event| matches!(&event.input, TerminalInput::Paste(text) if text == CLIPBOARD_TEXT));
    drop(subscription);
    if arrived {
        Ok(())
    } else {
        Err(format!(
            "the focused pane received no paste: {:?}",
            requests.borrow()
        )
        .into())
    }
}
