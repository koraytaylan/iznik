//! Owned input requests and mode-aware encoding on the terminal's owning thread.

use libghostty_vt::terminal::{Mode, Terminal};
use libghostty_vt::{key, mouse, paste};

use crate::grid::{GridPosition, GridSelection};
use crate::vt::{PaneKey, TerminalSnapshot, Viewport, VtError, VtOutput};
use iznik_protocol::identity::Sequence;

/// Two six-byte bracketed-paste delimiters are the encoder's maximum expansion.
const PASTE_OVERHEAD: usize = 12;

/// Keyboard data copied from a platform event without retaining any UI handle.
#[derive(Clone, Debug)]
pub struct KeyInput {
    /// Physical key identity used for function, cursor and keypad sequences.
    pub key: key::Key,
    /// Press, repeat or release, retained for extended keyboard protocols.
    pub action: key::Action,
    /// Modifiers physically held for the event.
    pub modifiers: key::Mods,
    /// Modifiers consumed by the keyboard layout while producing the text.
    pub consumed: key::Mods,
    /// Layout-produced text, which may contain multiple Unicode characters.
    pub text: String,
    /// Layout character before Shift, required to disambiguate modified letters.
    pub unshifted: Option<char>,
}

/// Mouse data in surface pixels, with the geometry used to draw that surface.
#[derive(Clone, Debug)]
pub struct MouseInput {
    /// Press, motion or release; wheel steps are presses of wheel buttons.
    pub action: mouse::Action,
    /// Button identity, absent for motion with no button held.
    pub button: Option<mouse::Button>,
    /// Keyboard modifiers held during the pointer event.
    pub modifiers: key::Mods,
    /// Position relative to the terminal surface.
    pub position: mouse::Position,
    /// Display geometry used by both cell and pixel mouse protocols.
    pub geometry: mouse::EncoderSize,
    /// Whether a pointer button is currently held, for drag-only tracking.
    pub pressed: bool,
}

/// Identity and geometry of the frame against which a local gesture was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputFrame {
    /// Host-qualified pane whose cells the surface displays.
    pub key: PaneKey,
    /// Displayed byte position, used to reject a changed frame.
    pub sequence: Sequence,
    /// Displayed width, used to reject a resize racing the request.
    pub columns: u16,
    /// Displayed history position and dimensions.
    pub viewport: Viewport,
}

impl From<&TerminalSnapshot> for InputFrame {
    fn from(snapshot: &TerminalSnapshot) -> Self {
        Self {
            key: snapshot.key.clone(),
            sequence: snapshot.sequence,
            columns: snapshot.columns,
            viewport: snapshot.viewport,
        }
    }
}

/// Selection and the displayed frame whose cells the clipboard must represent.
#[derive(Clone, Debug)]
pub struct CopyInput {
    /// Half-open cell selection, in either direction.
    pub selection: GridSelection,
    /// Frame identity shared with pointer gesture validation.
    pub frame: InputFrame,
}

/// Local action to perform only when the live terminal has not requested mouse input.
#[derive(Clone, Debug)]
pub enum PointerAction {
    /// Cell under a left-button press, drag or release; phase comes from the mouse event.
    Select(GridPosition),
    /// Complete history rows accumulated from the platform wheel event.
    Scroll(isize),
}

/// A platform pointer event with a frame-bound local fallback.
#[derive(Clone, Debug)]
pub struct PointerInput {
    /// Native encoder data; only the VT owner decides whether the program consumes it.
    pub mouse: MouseInput,
    /// Optional local selection or history action when mouse tracking is disabled.
    pub local: Option<PointerAction>,
    /// Displayed frame at event time, preventing delayed gestures from selecting new cells.
    pub frame: InputFrame,
}

/// A request whose encoding must use the pane's live terminal mode.
#[derive(Clone, Debug)]
pub enum TerminalInput {
    /// Serialize a range through the emulator, without sending terminal input.
    Copy(CopyInput),
    /// A keyboard event, including layout metadata and release/repeat state.
    Key(KeyInput),
    /// Clipboard text sanitized and framed by the native paste encoder.
    Paste(String),
    /// A pointer event, suppressed when the terminal has not requested it.
    Mouse(MouseInput),
    /// Platform pointer input with a local fallback selected by live terminal modes.
    Pointer(PointerInput),
}

/// Reusable native encoders, created and kept beside one non-Send terminal.
pub(crate) struct InputEncoder {
    /// The terminal library's complete keyboard protocol implementation.
    keyboard: key::Encoder<'static>,
    /// Mouse encoder retains the last reported cell between motion events.
    mouse: mouse::Encoder<'static>,
    /// Configured bound on encoded reports from one platform wheel request.
    maximum_wheel_reports: usize,
}

impl InputEncoder {
    /// Allocate encoders on the same thread that owns the terminal.
    ///
    /// # Errors
    /// Returns an emulator allocation error.
    pub(crate) fn new(maximum_wheel_reports: usize) -> Result<Self, VtError> {
        Ok(Self {
            keyboard: key::Encoder::new()?,
            mouse: mouse::Encoder::new()?,
            maximum_wheel_reports,
        })
    }

    /// Refresh terminal modes before encoding each ordered input request.
    ///
    /// # Errors
    /// Returns invalid pointer geometry, paste length overflow or emulator errors.
    pub(crate) fn encode(
        &mut self,
        terminal: &Terminal<'_, '_>,
        input: &TerminalInput,
    ) -> Result<VtOutput, VtError> {
        let bytes = match input {
            TerminalInput::Copy(input) => {
                return copy_selection(terminal, input.selection).map(VtOutput::Clipboard);
            }
            TerminalInput::Key(input) => self.key(terminal, input)?,
            TerminalInput::Paste(text) => encode_paste(terminal, text)?,
            TerminalInput::Mouse(input) => self.mouse(terminal, input)?,
            TerminalInput::Pointer(input) => return self.pointer(terminal, input),
        };
        Ok(VtOutput::Input(bytes))
    }

    /// Decide local fallback from tracking state, never from an empty encoded event.
    ///
    /// # Errors
    /// Returns native failures, invalid geometry or a wheel burst beyond its configured cap.
    fn pointer(
        &mut self,
        terminal: &Terminal<'_, '_>,
        input: &PointerInput,
    ) -> Result<VtOutput, VtError> {
        let bytes = self.mouse(terminal, &input.mouse)?;
        if !terminal.is_mouse_tracking()? && input.local.is_some() {
            return Ok(VtOutput::LocalPointer(Box::new(input.clone())));
        }
        let reports = match input.local {
            Some(PointerAction::Scroll(rows)) => rows.unsigned_abs(),
            _ => 1,
        };
        if matches!(input.local, Some(PointerAction::Scroll(_)))
            && reports > self.maximum_wheel_reports
        {
            return Err(VtError::Input(
                "wheel burst exceeds the configured report limit",
            ));
        }
        if bytes.is_empty() || reports == 1 {
            return Ok(VtOutput::Input(bytes));
        }
        let length = bytes.len().checked_mul(reports).ok_or(VtError::Overflow)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(length)
            .map_err(|_allocation| VtError::Input("wheel report allocation failed"))?;
        for _ in 0..reports {
            output.extend_from_slice(&bytes);
        }
        Ok(VtOutput::Input(output))
    }

    /// Preserve layout text and the physical event while the emulator selects bytes.
    ///
    /// # Errors
    /// Returns native event allocation or encoding errors.
    fn key(&mut self, terminal: &Terminal<'_, '_>, input: &KeyInput) -> Result<Vec<u8>, VtError> {
        let mut event = key::Event::new()?;
        event
            .set_action(input.action)
            .set_key(input.key)
            .set_mods(input.modifiers)
            .set_consumed_mods(input.consumed);
        if !input.text.is_empty() {
            event.set_utf8(Some(input.text.clone()));
        }
        if let Some(character) = input.unshifted {
            event.set_unshifted_codepoint(character);
        }
        let mut bytes = Vec::new();
        self.keyboard
            .set_options_from_terminal(terminal)
            .encode_to_vec(&event, &mut bytes)?;
        Ok(bytes)
    }

    /// Apply surface geometry and button state before native mouse encoding.
    ///
    /// # Errors
    /// Returns invalid geometry or native allocation and encoding errors.
    fn mouse(
        &mut self,
        terminal: &Terminal<'_, '_>,
        input: &MouseInput,
    ) -> Result<Vec<u8>, VtError> {
        if input.geometry.cell_width == 0
            || input.geometry.cell_height == 0
            || input.geometry.screen_width == 0
            || input.geometry.screen_height == 0
            || !input.position.x.is_finite()
            || !input.position.y.is_finite()
        {
            return Err(VtError::Input("invalid mouse surface geometry"));
        }
        let mut event = mouse::Event::new()?;
        event
            .set_action(input.action)
            .set_button(input.button)
            .set_mods(input.modifiers)
            .set_position(input.position);
        let mut bytes = Vec::new();
        self.mouse
            .set_options_from_terminal(terminal)
            .set_size(input.geometry)
            .set_any_button_pressed(input.pressed)
            .encode_to_vec(&event, &mut bytes)?;
        Ok(bytes)
    }
}

/// Sanitize control bytes before adding delimiters so pasted text cannot end its frame.
///
/// # Errors
/// Returns length overflow, unavailable terminal mode or native encoder errors.
fn encode_paste(terminal: &Terminal<'_, '_>, text: &str) -> Result<Vec<u8>, VtError> {
    let mut text = text.as_bytes().to_vec();
    let capacity = text
        .len()
        .checked_add(PASTE_OVERHEAD)
        .ok_or(VtError::Overflow)?;
    let mut bytes = vec![0; capacity];
    let written = paste::encode(&mut text, terminal.mode(Mode::BRACKETED_PASTE)?, &mut bytes)?;
    bytes.truncate(written);
    Ok(bytes)
}

/// Serialize half-open viewport cells with the native plain-text selection formatter.
///
/// # Errors
/// Returns invalid coordinates, native selection errors or unexpected invalid UTF-8.
fn copy_selection(
    terminal: &Terminal<'_, '_>,
    selection: GridSelection,
) -> Result<String, VtError> {
    use libghostty_vt::fmt::Format;
    use libghostty_vt::selection::{FormatOptions, Selection};
    use libghostty_vt::terminal::{Point, PointCoordinate};

    let columns = terminal.cols()?;
    let rows = terminal.rows()?;
    let mut start = selection.anchor.min(selection.head);
    let end = selection.anchor.max(selection.head);
    if start.column > columns || end.column > columns || start.row >= rows || end.row >= rows {
        return Err(VtError::Input(
            "selection is outside the displayed viewport",
        ));
    }
    if start == end {
        return Ok(String::new());
    }
    if start.column == columns {
        start = GridPosition {
            row: start.row.saturating_add(1),
            column: 0,
        };
    }
    if start >= end {
        return Ok(String::new());
    }
    let end = if end.column == 0 {
        GridPosition {
            row: end.row.saturating_sub(1),
            column: columns.saturating_sub(1),
        }
    } else {
        GridPosition {
            row: end.row,
            column: end.column.saturating_sub(1),
        }
    };
    let start = terminal.grid_ref(Point::Viewport(PointCoordinate {
        x: start.column,
        y: u32::from(start.row),
    }))?;
    let end = terminal.grid_ref(Point::Viewport(PointCoordinate {
        x: end.column,
        y: u32::from(end.row),
    }))?;
    let native = Selection::new(start, end, false);
    let options = FormatOptions::new()
        .with_emit_format(Format::Plain)
        .with_unwrap(true)
        .with_selection(&native);
    let Some(bytes) = terminal.format_selection_alloc(None, options)? else {
        return Ok(String::new());
    };
    String::from_utf8(bytes.to_vec())
        .map_err(|_invalid| VtError::Input("selection formatter returned invalid UTF-8"))
}
