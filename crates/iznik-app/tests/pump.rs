//! The window's pump wakes on what its owners queue, not on a timer.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{TestAppContext, WindowHandle};
use iznik_app::bridge::EngineEvent;
use iznik_app::vt::{PaneKey, VtOptions, VtThread};
use iznik_app::wake::WakeSignal;
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::{Generation, PaneId, Sequence, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

#[path = "support/engine.rs"]
mod engine;

/// Fixture setup and window failures.
type Failed = Box<dyn std::error::Error>;
/// The only pane of the fixture's one tab.
const PANE: PaneId = PaneId(1);
/// Width of the fixture pane.
const COLUMNS: u16 = 20;
/// Height of the fixture pane.
const ROWS: u16 = 2;
/// A fallback this long never fires inside the test, so only a wakeup can
/// deliver the frame.
const NEVER: Duration = Duration::from_hours(1);
/// A missing wakeup fails promptly rather than hanging the headless window.
const DEADLINE: Duration = Duration::from_secs(2);
/// Yield to the emulator thread between executor turns.
const INTERVAL: Duration = Duration::from_millis(1);

/// Keep fixture assertion outside the GPUI macro's generated test documentation.
///
/// # Panics
/// Fails on a fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// The pane every event in this file addresses.
fn key() -> PaneKey {
    PaneKey {
        host: HostId("fixture".to_owned()),
        pane: PANE,
    }
}

/// One session holding one tab with one pane.
fn model() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                layout: LayoutNode::Leaf(PANE),
                panes: vec![Pane {
                    id: PANE,
                    title: String::new(),
                    working_directory: None,
                    columns: COLUMNS,
                    rows: ROWS,
                }],
            }],
        }],
    }
}

/// A raise with nobody waiting wakes the next wait.
///
/// # Panics
/// Fails when the kept raise does not resolve the wait.
#[test]
fn raised_signal_is_kept_until_waited() {
    let signal = WakeSignal::new();
    signal.raise();
    let thread = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .map(|runtime| runtime.block_on(signal.raised()))
            .is_ok()
    });
    assert!(
        thread.join().unwrap_or(false),
        "a raise before the wait resolves it"
    );
}

#[gpui_kit::test]
fn pump_draws_output_before_its_fallback(context: &mut TestAppContext) {
    check(&woken(context));
}

/// Feed a screen to a shell whose fallback never fires, and wait for its frame
/// without calling `update`.
///
/// # Errors
/// Propagates fixture, encoding and window failures, and a frame that does not
/// arrive before the deadline.
fn woken(context: &mut TestAppContext) -> Result<(), Failed> {
    // The emulator thread raises the pump's signal. The test scheduler records
    // a wake from any other thread as non-determinism and fails the test at
    // the end, which is what a slow run hits once the pump is already waiting.
    // Allowing parking keeps that wake, and the hour-long fallback still cannot
    // be what delivers the frame: this loop never advances the test clock.
    context.background_executor.allow_parking();
    context.update(gpui_kit::init);
    let (bridge, _directory) = engine::start("pump")?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, build| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: Some(NEVER),
                ..ShellOptions::default()
            },
            window,
            build,
        )
    });
    absorb(
        context,
        handle,
        ManagerEvent::Snapshot {
            host: key().host,
            generation: Generation(1),
            payload: encode_host_model(&model())?,
        },
    )?;
    absorb(
        context,
        handle,
        ManagerEvent::Screen {
            host: key().host,
            pane: PANE,
            sequence: Sequence(0),
            columns: COLUMNS,
            rows: ROWS,
            bytes: b"woken".to_vec(),
        },
    )?;
    let started = Instant::now();
    loop {
        context.run_until_parked();
        let shown = handle.update(context, |shell, _window, application| {
            shell.surface(&key()).is_some_and(|surface| {
                surface
                    .read(application)
                    .grid()
                    .read(application)
                    .snapshot()
                    .is_some()
            })
        })?;
        if shown {
            return Ok(());
        }
        if started.elapsed() >= DEADLINE {
            return Err("the pump did not wake for the emulator's frame".into());
        }
        std::thread::sleep(INTERVAL);
    }
}

/// Submit an event through the shell's single ingress path.
///
/// # Errors
/// Returns a closed-window failure.
fn absorb(
    context: &mut TestAppContext,
    handle: WindowHandle<WindowShell>,
    event: ManagerEvent,
) -> Result<(), Failed> {
    handle.update(context, |shell, window, application| {
        shell.absorb(EngineEvent::Said(event), window, application);
    })?;
    Ok(())
}
