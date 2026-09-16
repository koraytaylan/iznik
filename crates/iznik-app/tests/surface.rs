//! Per-pane GPUI routes with the real VT owner and an isolated, unattached engine.

#[path = "support/engine.rs"]
mod engine;
mod support;

use std::rc::Rc;

use gpui_kit::{TestAppContext, WindowHandle};
use iznik_app::bridge::EngineBridge;
use iznik_app::grid::{GridMetrics, GridPosition, GridSelection};
use iznik_app::surface::{PaneSurface, SurfaceError};
use iznik_app::vt::{VtCommand, VtEvent, VtOptions, VtOutput, VtThread};
use iznik_protocol::identity::{PaneId, Sequence};
use libghostty_vt::terminal::ScrollViewport;

/// Errors propagated by the native owner, engine or GPUI fixture.
type Failed = Box<dyn std::error::Error>;
/// Narrow fixture leaves multiple history rows after a short screen replay.
const COLUMNS: u16 = 8;
/// Two displayed rows distinguish history from the current viewport.
const ROWS: u16 = 2;
/// A distinct pane tests rejection before clipboard side effects.
const OTHER_PANE: u64 = 2;

/// A surface, its shared VT owner and an engine with no host connections.
struct Fixture {
    /// Real GPUI window containing the production per-pane wiring.
    handle: WindowHandle<PaneSurface>,
    /// Engine operations deliberately reject the unattached synthetic host.
    bridge: EngineBridge,
    /// The same owner receives UI requests and supplies snapshots.
    thread: Rc<VtThread>,
    /// Removed after the engine drops its runtime and logging handles.
    _directory: engine::Directory,
}

impl Fixture {
    /// Start only the local owners; no daemon or remote host is involved.
    ///
    /// # Errors
    /// Propagates filesystem, engine and VT startup failures.
    ///
    /// # Panics
    /// Fails if the owning thread misses the fixture reply deadline.
    fn new(context: &mut TestAppContext, name: &str) -> Result<Self, Failed> {
        context.update(gpui_kit::init);
        let (bridge, directory) = engine::start(name)?;
        let thread = Rc::new(VtThread::start(VtOptions::default())?);
        let handle = context.add_window(|_, context| {
            PaneSurface::new(
                support::key(),
                GridMetrics::default(),
                Rc::clone(&thread),
                context,
            )
        });
        let snapshot = support::open(&thread, Sequence(0), COLUMNS, ROWS)?;
        handle.update(context, |surface, _, context| {
            surface.receive(
                VtEvent {
                    key: support::key(),
                    result: Ok(Some(VtOutput::Snapshot(Box::new(snapshot)))),
                },
                &bridge,
                context,
            )
        })??;
        Ok(Self {
            handle,
            bridge,
            thread,
            _directory: directory,
        })
    }

    /// Consume exactly the next owning-thread result through the production surface.
    ///
    /// # Errors
    /// Returns a closed window, invalid reply or failed engine operation.
    ///
    /// # Panics
    /// Fails if the owning thread misses the fixture reply deadline.
    fn receive(&self, context: &mut TestAppContext) -> Result<(), Failed> {
        let event = support::receive(&self.thread);
        self.handle.update(context, |surface, _, context| {
            surface.receive(event, &self.bridge, context)
        })??;
        Ok(())
    }

    /// Establish terminal text without earning stream credit.
    ///
    /// # Errors
    /// Propagates owner submission and surface consumption failures.
    ///
    /// # Panics
    /// Fails if the owning thread misses the fixture reply deadline.
    fn screen(&self, context: &mut TestAppContext, bytes: &[u8]) -> Result<(), Failed> {
        self.thread.send(VtCommand::Screen {
            key: support::key(),
            sequence: Sequence(0),
            columns: COLUMNS,
            rows: ROWS,
            bytes: bytes.to_vec(),
            theme: Box::default(),
        })?;
        self.receive(context)
    }
}

/// Verify the production surface route.
///
/// # Panics
/// Fails if setup or the asserted behavior differs.
#[gpui_kit::test]
fn surface_routes_copy_and_history_through_grid_subscriptions(context: &mut TestAppContext) {
    check(&copy_and_history(context));
}

/// Selection copy and history use retained production subscriptions and the real clipboard.
///
/// # Errors
/// Propagates fixture and surface failures.
///
/// # Panics
/// Fails on incorrect clipboard text, history position or cross-pane side effects.
fn copy_and_history(context: &mut TestAppContext) -> Result<(), Failed> {
    let fixture = Fixture::new(context, "clipboard")?;
    fixture.screen(context, b"old\r\nhello\r\nworld")?;
    fixture.handle.update(context, |surface, _, context| {
        surface.grid().update(context, |grid, context| {
            grid.select(
                Some(GridSelection {
                    anchor: GridPosition { row: 0, column: 0 },
                    head: GridPosition { row: 0, column: 1 },
                }),
                context,
            )?;
            grid.copy_selection(context);
            Ok::<_, Failed>(())
        })
    })??;
    fixture.receive(context)?;
    assert_eq!(
        context.read(|context| context.read_from_clipboard().and_then(|item| item.text())),
        Some("h".to_owned()),
        "clipboard contains the selected native text"
    );
    fixture.handle.update(context, |surface, _, context| {
        surface.grid().update(context, |grid, context| {
            grid.scroll(ScrollViewport::Top, context);
        });
    })?;
    fixture.receive(context)?;
    fixture.handle.update(context, |surface, _, context| {
        let snapshot = surface
            .grid()
            .read(context)
            .snapshot()
            .ok_or("surface frame")?;
        assert_eq!(snapshot.viewport.offset, 0, "history reached the top");
        assert_eq!(
            snapshot
                .rows
                .first()
                .and_then(|row| row.first())
                .map(|cell| cell.text.as_str()),
            Some("o"),
            "owner returned the first history row"
        );
        Ok::<_, Failed>(())
    })??;
    let mut key = support::key();
    key.pane = PaneId(OTHER_PANE);
    let rejected = fixture.handle.update(context, |surface, _, context| {
        surface.receive(
            VtEvent {
                key,
                result: Ok(Some(VtOutput::Clipboard("wrong".to_owned()))),
            },
            &fixture.bridge,
            context,
        )
    })?;
    assert!(
        matches!(rejected, Err(SurfaceError::Pane)),
        "foreign reply rejected"
    );
    assert_eq!(
        context.read(|context| context.read_from_clipboard().and_then(|item| item.text())),
        Some("h".to_owned()),
        "clipboard contains the selected native text"
    );
    Ok(())
}

/// Verify the production surface route.
///
/// # Panics
/// Fails if setup or the asserted behavior differs.
#[gpui_kit::test]
fn surface_retains_consumed_credit_when_the_engine_rejects_submission(
    context: &mut TestAppContext,
) {
    check(&credit_failure(context));
}

/// A failed grant preserves the accepted display and remains available for retry.
///
/// # Errors
/// Propagates fixture, owner and window failures.
///
/// # Panics
/// Fails if credit is discarded, duplicated, or returned for the initial screen.
fn credit_failure(context: &mut TestAppContext) -> Result<(), Failed> {
    let fixture = Fixture::new(context, "credit")?;
    fixture.screen(context, b"initial")?;
    let bytes = b"\r\nnext";
    fixture.thread.send(VtCommand::Feed {
        receipt: None,
        key: support::key(),
        sequence: Sequence(0),
        bytes: bytes.to_vec(),
    })?;
    let event = support::receive(&fixture.thread);
    let result = fixture.handle.update(context, |surface, _, context| {
        surface.receive(event, &fixture.bridge, context)
    })?;
    assert!(
        matches!(result, Err(SurfaceError::Engine(_))),
        "unattached engine refuses the grant"
    );
    fixture.handle.update(context, |surface, _, context| {
        assert_eq!(
            surface
                .grid()
                .read(context)
                .snapshot()
                .ok_or("accepted frame")?
                .sequence,
            Sequence(u64::try_from(bytes.len())?),
            "failed credit does not undo frame consumption"
        );
        assert!(
            matches!(
                surface.flush_credit(&fixture.bridge, context),
                Err(SurfaceError::Engine(_))
            ),
            "retry still owns the unsubmitted grant"
        );
        let mut grants = Vec::new();
        surface.grid().update(context, |grid, _| {
            grid.flush_credit(|key, count| {
                grants.push((key.clone(), count));
                Ok::<_, Failed>(())
            })?;
            grid.flush_credit(|_, _| Err::<(), Failed>("duplicate credit".into()))
        })?;
        assert_eq!(
            grants,
            vec![(support::key(), u32::try_from(bytes.len())?)],
            "the retry returns the exact credit once"
        );
        Ok::<_, Failed>(())
    })??;
    Ok(())
}

/// Verify both destinations of owned input through retained grid subscriptions.
///
/// # Panics
/// Fails if event delivery, native mode resolution or surface routing differs.
#[gpui_kit::test]
fn surface_routes_pointer_fallback_and_encoded_process_input(context: &mut TestAppContext) {
    check(&pointer_and_input(context));
}

/// Pointer fallback becomes selection, while paste bytes are offered to the engine.
///
/// # Errors
/// Propagates fixture, window and native-owner failures.
///
/// # Panics
/// Fails if either destination is lost or routed as the other.
fn pointer_and_input(context: &mut TestAppContext) -> Result<(), Failed> {
    use iznik_app::grid::GridInput;
    use iznik_app::input::{InputFrame, MouseInput, PointerAction, PointerInput, TerminalInput};
    use libghostty_vt::mouse::{Action, Button, EncoderSize, Position};
    let fixture = Fixture::new(context, "pointer")?;
    fixture.screen(context, b"hello")?;
    for action in [Action::Press, Action::Motion, Action::Release] {
        fixture.handle.update(context, |surface, _, context| {
            surface.grid().update(context, |grid, context| {
                let frame = InputFrame::from(grid.snapshot().ok_or("displayed frame")?);
                context.emit(GridInput {
                    key: support::key(),
                    input: TerminalInput::Pointer(PointerInput {
                        frame,
                        local: Some(PointerAction::Select(GridPosition { row: 0, column: 0 })),
                        mouse: MouseInput {
                            action,
                            button: Some(Button::Left),
                            modifiers: libghostty_vt::key::Mods::empty(),
                            position: Position { x: 0.0, y: 0.0 },
                            geometry: EncoderSize {
                                screen_width: u32::from(COLUMNS),
                                screen_height: u32::from(ROWS),
                                cell_width: 1,
                                cell_height: 1,
                                padding_top: 0,
                                padding_bottom: 0,
                                padding_left: 0,
                                padding_right: 0,
                            },
                            pressed: true,
                        },
                    }),
                });
                Ok::<_, Failed>(())
            })
        })??;
        fixture.receive(context)?;
    }
    fixture.handle.update(context, |surface, _, context| {
        surface
            .grid()
            .update(context, |grid, context| grid.copy_selection(context));
    })?;
    fixture.receive(context)?;
    assert_eq!(
        context.read(|context| context.read_from_clipboard().and_then(|item| item.text())),
        Some("h".to_owned()),
        "the owner's local pointer fallback reached selection"
    );
    fixture.handle.update(context, |surface, _, context| {
        surface.grid().update(context, |_, context| {
            context.emit(GridInput {
                key: support::key(),
                input: TerminalInput::Paste("typed".to_owned()),
            });
        });
    })?;
    let event = support::receive(&fixture.thread);
    assert!(
        matches!(&event.result, Ok(Some(VtOutput::Input(bytes))) if bytes == b"typed"),
        "the grid subscription reached the native input encoder"
    );
    let result = fixture.handle.update(context, |surface, _, context| {
        surface.receive(event, &fixture.bridge, context)
    })?;
    assert!(
        matches!(result, Err(SurfaceError::Engine(_))),
        "process input reaches the engine, which has no attached host in this fixture"
    );
    Ok(())
}

/// Report fixture failures outside the GPUI macro, which replaces test documentation.
///
/// # Panics
/// Fails when the fixture or its behavior checks return an error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}
