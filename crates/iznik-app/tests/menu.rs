//! The application installs a main menu, so the system menu bar describes
//! iznik rather than whatever application was frontmost before it, and the
//! menu's terminal items act on the focused pane.

#[path = "support/engine.rs"]
mod engine;

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{
    App, AppContext as _, ClipboardItem, Entity, Focusable, Subscription, TestAppContext,
};
use iznik_app::grid::{GridInput, GridMetrics, TerminalGrid};
use iznik_app::host_ui::Notice;
use iznik_app::input::TerminalInput;
use iznik_app::menu;
use iznik_app::vt::{VtCommand, VtOptions, VtOutput, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::{Generation, PaneId, Sequence, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

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

/// Tab completes a path in the shell instead of moving focus out of the pane.
#[gpui_kit::test]
fn tab_reaches_the_focused_pane(context: &mut TestAppContext) {
    check(&tab_reaches(context));
}

/// Command-W closes the focused pane. A running program does not turn that
/// into a request to close the whole tab.
#[gpui_kit::test]
fn close_command_closes_the_focused_pane(context: &mut TestAppContext) {
    check(&close_pane(context));
}

/// Command-T asks the host for a new tab in the session on screen.
#[gpui_kit::test]
fn new_tab_command_asks_the_host_for_a_tab(context: &mut TestAppContext) {
    check(&new_tab(context));
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

/// Focus a pane inside the window root and press Tab.
///
/// The root binds Tab to focus movement. The pane must still receive the
/// completion byte, and Shift-Tab must still request the previous completion.
///
/// # Errors
/// Returns emulator, window or assertion failures.
///
/// # Panics
/// Fails when the thread misses a reply deadline.
fn tab_reaches(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| {
        gpui_kit::init(app);
        menu::install(app);
    });
    let thread = VtThread::start(VtOptions::default())?;
    let frame = support::open(&thread, Sequence(0), COLUMNS, ROWS)?;
    let requests = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&requests);
    let grid_slot: Rc<RefCell<Option<Entity<TerminalGrid>>>> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&grid_slot);
    let root = context.add_window(|window, context| {
        let grid = context.new(|context| TerminalGrid::new(GridMetrics::default(), context));
        *slot.borrow_mut() = Some(grid.clone());
        gpui_kit::component::Root::new(grid, window, context)
    });
    let grid = grid_slot
        .borrow()
        .clone()
        .ok_or("the window has no terminal")?;
    let mut subscription = None;
    let focus = grid.update(context, |grid, context| {
        grid.apply(frame, context)?;
        subscription = Some(context.subscribe(
            &context.entity(),
            move |_, _, event: &GridInput, _| {
                observed.borrow_mut().push(event.clone());
            },
        ));
        Ok::<_, Failed>(grid.focus_handle(context))
    })?;
    context.update_window(root.into(), |_, window, application| {
        window.focus(&focus, application);
        window.draw(application).clear(application);
    })?;
    context.simulate_keystrokes(root.into(), "tab");
    let plain = encoded(&thread, &requests)?;
    if plain.as_slice() != b"\t" {
        return Err(format!("tab encoded {plain:?}").into());
    }
    context.simulate_keystrokes(root.into(), "shift-tab");
    let reverse = encoded(&thread, &requests)?;
    if reverse.as_slice() != b"\x1b[Z" {
        return Err(format!("shift-tab encoded {reverse:?}").into());
    }
    let still = context.update_window(root.into(), |_, window, application| {
        window.focused(application).as_ref() == Some(&focus)
    })?;
    drop(subscription);
    if still {
        Ok(())
    } else {
        Err("tab moved focus out of the pane".into())
    }
}

/// The second tab is the one Command-2 opens, and the one whose pane Command-W closes.
const RUNNING_TAB: TabId = TabId(2);
/// The program that makes closing that tab ask first.
const RUNNING_PROGRAM: &str = "vim";

/// The pane Command-T must not type into.
fn open_pane() -> iznik_app::vt::PaneKey {
    iznik_app::vt::PaneKey {
        host: HostId("build".to_owned()),
        pane: PaneId(11),
    }
}

/// Press Command-T on a pane that already has a screen, and record the refusal.
///
/// The fixture host is not held, so a tab command is refused at once. That
/// refusal is the evidence the chord asked for a tab rather than typing `t`.
///
/// # Errors
/// Returns fixture, window or assertion failures.
fn new_tab(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| {
        gpui_kit::init(app);
        menu::install(app);
    });
    let (bridge, _directory) = engine::start("menu-new-tab")?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, context| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ..ShellOptions::default()
            },
            window,
            context,
        )
    });
    let notices = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&notices);
    let typed = Rc::new(RefCell::new(Vec::new()));
    let typed_observed = Rc::clone(&typed);
    let mut subscriptions = Vec::new();
    handle.update(context, |shell, window, context| {
        let host = open_pane().host;
        shell.absorb(
            iznik_app::bridge::EngineEvent::Said(ManagerEvent::Snapshot {
                host: host.clone(),
                generation: Generation(1),
                payload: encode_host_model(&running_tabs())?,
            }),
            window,
            context,
        );
        shell.absorb(
            iznik_app::bridge::EngineEvent::Said(ManagerEvent::Screen {
                host,
                pane: open_pane().pane,
                sequence: Sequence(0),
                columns: 80,
                rows: 24,
                bytes: b"ready".to_vec(),
            }),
            window,
            context,
        );
        subscriptions.push(context.subscribe(
            &context.entity(),
            move |_, _, notice: &Notice, _| {
                observed.borrow_mut().push(notice.detail.clone());
            },
        ));
        if let Some(surface) = shell.surface(&open_pane()) {
            let grid = surface.read(context).grid().clone();
            subscriptions.push(context.subscribe(&grid, move |_, _, event: &GridInput, _| {
                typed_observed.borrow_mut().push(event.clone());
            }));
        }
        Ok::<(), Failed>(())
    })??;
    let started = std::time::Instant::now();
    loop {
        let ready = handle.update(context, |shell, window, app| {
            shell.update(window, app);
            shell
                .surface(&open_pane())
                .is_some_and(|surface| surface.read(app).grid().read(app).snapshot().is_some())
        })?;
        if ready {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(2) {
            return Err("the pane never showed its screen".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    context.simulate_keystrokes(handle.into(), "cmd-t");
    drop(subscriptions);
    if !typed.borrow().is_empty() {
        return Err(format!("command-t reached the terminal: {:?}", typed.borrow()).into());
    }
    if notices
        .borrow()
        .iter()
        .any(|detail| detail.contains("not held"))
    {
        Ok(())
    } else {
        Err(format!("command-t did not ask for a tab: {:?}", notices.borrow()).into())
    }
}

/// Open a shell on an idle tab and a running one, move to the running tab,
/// record its pane as focused, and press Command-W.
///
/// The fixture host is not held, so the close is refused at once. That refusal
/// is the evidence the chord asked the host to close the pane. A close of the
/// whole tab would have asked first, because the pane is running a program.
///
/// # Errors
/// Returns fixture, window or assertion failures.
fn close_pane(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(|app| {
        gpui_kit::init(app);
        menu::install(app);
    });
    let (bridge, _directory) = engine::start("menu-close")?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, context| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ..ShellOptions::default()
            },
            window,
            context,
        )
    });
    let notices = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&notices);
    let mut subscription = None;
    handle.update(context, |shell, window, context| {
        shell.absorb(
            iznik_app::bridge::EngineEvent::Said(ManagerEvent::Snapshot {
                host: HostId("build".to_owned()),
                generation: Generation(1),
                payload: encode_host_model(&running_tabs())?,
            }),
            window,
            context,
        );
        subscription = Some(context.subscribe(
            &context.entity(),
            move |_, _, notice: &Notice, _| {
                observed.borrow_mut().push(notice.detail.clone());
            },
        ));
        let refused = shell.hosts_mut().focus("build", Some(PaneId(12)));
        if refused.is_ok() {
            return Err("an unheld host accepted focus".into());
        }
        Ok::<(), Failed>(())
    })??;
    context.update_window(handle.into(), |_, window, application| {
        window.draw(application).clear(application);
    })?;
    context.simulate_keystrokes(handle.into(), "cmd-2");
    let selected = handle.update(context, |shell, _, _| shell.selected().map(|key| key.tab))?;
    if selected != Some(RUNNING_TAB) {
        return Err(format!("command-2 left {selected:?} on screen").into());
    }
    context.simulate_keystrokes(handle.into(), "cmd-w");
    drop(subscription);
    handle.update(context, |shell, _, _| -> Result<(), Failed> {
        if shell.palette().prompt.is_some() {
            return Err("command-w asked a question instead of closing the pane".into());
        }
        let recorded = shell
            .hosts()
            .state()
            .model()
            .host(&HostId("build".to_owned()))
            .and_then(|view| view.focus);
        if recorded != Some(PaneId(12)) {
            return Err(format!("focus was {recorded:?}, not the running pane").into());
        }
        if notices
            .borrow()
            .iter()
            .any(|detail| detail.contains("is not held"))
        {
            Ok(())
        } else {
            Err(format!(
                "command-w did not ask the host to close the pane: {:?}",
                notices.borrow()
            )
            .into())
        }
    })?
}

/// An idle shell and a tab running vim, in that order.
fn running_tabs() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![
                one_tab(TabId(1), PaneId(11), "zsh"),
                one_tab(RUNNING_TAB, PaneId(12), RUNNING_PROGRAM),
            ],
        }],
    }
}

/// One leaf tab whose pane title is `title`.
fn one_tab(tab: TabId, pane: PaneId, title: &str) -> Tab {
    Tab {
        id: tab,
        name: format!("tab {}", tab.0),
        panes: vec![Pane {
            id: pane,
            title: title.to_owned(),
            working_directory: None,
            columns: 80,
            rows: 24,
        }],
        layout: LayoutNode::Leaf(pane),
    }
}

/// Drain the pane's queued input through the native encoder.
///
/// # Errors
/// Returns a thread failure or a reply that is not encoded input.
///
/// # Panics
/// Fails when the thread misses a reply deadline.
fn encoded(thread: &VtThread, requests: &Rc<RefCell<Vec<GridInput>>>) -> Result<Vec<u8>, Failed> {
    let pending: Vec<_> = requests.borrow_mut().drain(..).collect();
    let mut encoded = Vec::new();
    for request in pending {
        thread.send(VtCommand::Input {
            key: request.key,
            input: request.input,
        })?;
        let reply = support::receive(thread)
            .result?
            .ok_or("missing input reply")?;
        let VtOutput::Input(bytes) = reply else {
            return Err("expected encoded input".into());
        };
        encoded.extend(bytes);
    }
    Ok(encoded)
}
