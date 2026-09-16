//! Per-pane wiring between GPUI grid events, the VT owner and the engine bridge.

use std::rc::Rc;

use gpui_kit::{
    App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, Subscription, Window,
};

use crate::bridge::{EngineBridge, EngineError};
use crate::grid::{GridError, GridInput, GridMetrics, GridScroll, TerminalGrid};
use crate::vt::{PaneKey, VtCommand, VtError, VtEvent, VtOutput, VtThread};

/// A surface operation failed and the window must show the failure for this pane.
#[derive(Clone, Debug)]
pub struct SurfaceFailure {
    /// Host-qualified pane that could not finish the operation.
    pub key: PaneKey,
    /// Error text from the component that rejected the operation.
    pub detail: String,
}

/// A terminal surface could not consume or forward a reply.
#[derive(Debug)]
pub enum SurfaceError {
    /// The window routed a reply to a different pane's surface.
    Pane,
    /// The engine refused input, a screen request or a credit grant.
    Engine(EngineError),
    /// The terminal owner rejected the request.
    Terminal(VtError),
    /// The grid rejected the frame or local gesture.
    Grid(GridError),
}

impl core::fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Pane => formatter.write_str("terminal reply belongs to another pane"),
            Self::Engine(error) => error.fmt(formatter),
            Self::Terminal(error) => error.fmt(formatter),
            Self::Grid(error) => error.fmt(formatter),
        }
    }
}

impl core::error::Error for SurfaceError {}

/// One pane's grid and its retained event routes, owned by the GPUI thread.
///
/// A transport disconnect does not destroy this entity or its emulator. The
/// window drops it only when it stops presenting the pane, and separately
/// decides whether that pane's emulator must survive for a later resume.
#[derive(Debug)]
pub struct PaneSurface {
    /// Identity used to reject misrouted output before any side effect.
    key: PaneKey,
    /// Cached grid entity, preserved while the surrounding layout changes.
    grid: Entity<TerminalGrid>,
    /// Keep input and history event listeners alive as long as the surface.
    _subscriptions: Vec<Subscription>,
}

impl PaneSurface {
    /// Connect one grid's owned events to the shared nonblocking VT command channel.
    pub fn new(
        key: PaneKey,
        metrics: GridMetrics,
        thread: Rc<VtThread>,
        context: &mut Context<'_, Self>,
    ) -> Self {
        let grid = context.new(|context| TerminalGrid::new(metrics, context));
        let input_thread = Rc::clone(&thread);
        let input = context.subscribe(&grid, move |surface, _, event: &GridInput, context| {
            surface.send(
                &input_thread,
                VtCommand::Input {
                    key: event.key.clone(),
                    input: event.input.clone(),
                },
                context,
            );
        });
        let scroll = context.subscribe(&grid, move |surface, _, event: &GridScroll, context| {
            surface.send(
                &thread,
                VtCommand::Scroll {
                    key: event.key.clone(),
                    scroll: event.scroll,
                },
                context,
            );
        });
        Self {
            key,
            grid,
            _subscriptions: vec![input, scroll],
        }
    }

    /// Stable grid entity used for selection actions and renderer observations.
    #[must_use]
    pub fn grid(&self) -> &Entity<TerminalGrid> {
        &self.grid
    }

    /// Consume a VT reply, forwarding process input before handling surface results.
    /// Snapshot credit is returned only after the grid accepts the frame.
    ///
    /// # Errors
    /// Rejects another pane's reply; otherwise returns the original engine,
    /// terminal or grid failure. Failed credit remains queued on the grid.
    pub fn receive(
        &mut self,
        event: VtEvent,
        bridge: &EngineBridge,
        context: &mut Context<'_, Self>,
    ) -> Result<(), SurfaceError> {
        if event.key != self.key {
            return Err(SurfaceError::Pane);
        }
        let output = bridge
            .terminal_event(event)
            .map_err(SurfaceError::Engine)?
            .map_err(SurfaceError::Terminal)?;
        match output {
            Some(VtOutput::Snapshot(snapshot)) => {
                self.grid
                    .update(context, |grid, context| grid.apply(*snapshot, context))
                    .map_err(SurfaceError::Grid)?;
            }
            Some(VtOutput::LocalPointer(input)) => {
                self.grid
                    .update(context, |grid, context| grid.apply_pointer(&input, context))
                    .map_err(SurfaceError::Grid)?;
            }
            Some(VtOutput::Clipboard(text)) => {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            // The bridge consumes encoded input, including empty filtered reports.
            Some(VtOutput::Input(_)) | None => {}
        }
        self.flush_credit(bridge, context)
    }

    /// Retry a retained credit grant when the engine can accept orders again.
    ///
    /// # Errors
    /// Returns the engine failure without discarding the grant.
    pub fn flush_credit(
        &self,
        bridge: &EngineBridge,
        context: &mut App,
    ) -> Result<(), SurfaceError> {
        self.grid
            .update(context, |grid, _| bridge.flush_terminal_credit(grid))
            .map_err(SurfaceError::Engine)
    }

    /// Report asynchronous event-submission failures through the same pane identity.
    fn send(&self, thread: &VtThread, command: VtCommand, context: &mut Context<'_, Self>) {
        if let Err(error) = thread.send(command) {
            context.emit(SurfaceFailure {
                key: self.key.clone(),
                detail: error.to_string(),
            });
        }
    }
}

impl EventEmitter<SurfaceFailure> for PaneSurface {}

impl Focusable for PaneSurface {
    fn focus_handle(&self, context: &App) -> FocusHandle {
        self.grid.read(context).focus_handle(context)
    }
}

impl Render for PaneSurface {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        self.grid.clone()
    }
}
