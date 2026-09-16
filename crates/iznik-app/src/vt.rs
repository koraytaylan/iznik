//! One `LocalSet` thread owns every application emulator. Only owned snapshots
//! and commands cross its channel; no terminal handle leaves the thread.

use crate::input::{InputEncoder, TerminalInput};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::credit::CreditReceipt;
use iznik_protocol::identity::{PaneId, Sequence};
use libghostty_vt::render::{
    CellIterator, Colors, CursorViewport, CursorVisualStyle, Dirty, RenderState, RowIterator,
};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::style::{Palette, RgbColor, Style};
use libghostty_vt::terminal::{
    ColorScheme, ConformanceLevel, DeviceAttributes, DeviceType, Options, PrimaryDeviceAttributes,
    ScrollViewport, SecondaryDeviceAttributes, SizeReportSize, Terminal, TertiaryDeviceAttributes,
};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;

/// Emulator history memory budget, matching the server's default budget.
pub const SCROLLBACK_BYTES: usize = 10_000;
/// Neutral foreground until application settings provide a theme.
const FOREGROUND: RgbColor = RgbColor {
    r: 216,
    g: 216,
    b: 216,
};
/// Dark background used by the initial application theme.
const BACKGROUND: RgbColor = RgbColor {
    r: 24,
    g: 24,
    b: 24,
};
/// Compile-time checked attributes; no variable-length feature list can panic.
const PRIMARY_ATTRIBUTES: PrimaryDeviceAttributes =
    PrimaryDeviceAttributes::new(ConformanceLevel::VT220, &[]);
/// Version reported by the terminal actually answering a program.
const VERSION: &str = concat!("iznik-app ", env!("CARGO_PKG_VERSION"));

/// Global pane identity: two hosts may both own pane number one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneKey {
    /// Host alias preserved exactly as entered.
    pub host: HostId,
    /// Identity assigned by that host.
    pub pane: PaneId,
}

/// Colors shared by the renderer and emulator query callbacks.
#[derive(Clone, Debug)]
pub struct TerminalTheme {
    /// Default text color, before program overrides.
    pub foreground: RgbColor,
    /// Default window background, before program overrides.
    pub background: RgbColor,
    /// Cursor color, when explicitly configured.
    pub cursor: Option<RgbColor>,
    /// Optional palette; absent uses the emulator's built-in palette.
    pub palette: Option<Palette>,
    /// Scheme programs receive when they query light versus dark.
    pub scheme: ColorScheme,
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self {
            foreground: FOREGROUND,
            background: BACKGROUND,
            cursor: None,
            palette: None,
            scheme: ColorScheme::Dark,
        }
    }
}

/// Resource limits for the thread, adjustable by a test.
#[derive(Clone, Debug)]
pub struct VtOptions {
    /// Per-pane history memory budget, in bytes rather than rows.
    pub scrollback_bytes: usize,
    /// Maximum native wheel reports emitted by one platform request; excess is refused.
    pub maximum_wheel_reports: usize,
}

/// A wheel burst stays bounded to a few kilobytes and cannot monopolize the VT owner.
const MAXIMUM_WHEEL_REPORTS: usize = 128;

impl Default for VtOptions {
    fn default() -> Self {
        Self {
            scrollback_bytes: SCROLLBACK_BYTES,
            maximum_wheel_reports: MAXIMUM_WHEEL_REPORTS,
        }
    }
}

/// One owned cell; style retains every emulator decoration for the renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSnapshot {
    /// Complete grapheme, including combining characters.
    pub text: String,
    /// Narrow, wide, or continuation status from the emulator.
    pub width: CellWide,
    /// Bold, italic, underline, and all other style flags.
    pub style: Style,
    /// Effective foreground, resolved through the active palette.
    pub foreground: RgbColor,
    /// Effective background, including background-only erased cells.
    pub background: RgbColor,
}

/// The emulator's current window into its retained rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// Total retained rows, including the active screen.
    pub total: u64,
    /// Row offset from the oldest retained row.
    pub offset: u64,
    /// Number of visible rows.
    pub rows: u64,
}

impl Viewport {
    /// Whether incoming output should keep the viewport at the active screen.
    #[must_use]
    pub fn at_bottom(self) -> bool {
        // Saturating arithmetic also treats an out-of-range offset as bottom.
        self.offset.saturating_add(self.rows) >= self.total
    }
}

/// Owned render state at an exact absolute pane byte position.
#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    /// Host-qualified pane identity.
    pub key: PaneKey,
    /// First byte not yet consumed by the emulator.
    pub sequence: Sequence,
    /// Live emulator width, including program-requested changes.
    pub columns: u16,
    /// Visible rows in display order, each containing exactly `columns` cells.
    pub rows: Vec<Vec<CellSnapshot>>,
    /// Visible cursor position, absent when outside the viewport or hidden.
    pub cursor: Option<CursorViewport>,
    /// Block, underline, or bar requested by the program.
    pub cursor_style: CursorVisualStyle,
    /// Active default colors and palette.
    pub colors: Colors,
    /// Damage state since the previous snapshot.
    pub dirty: Dirty,
    /// Per-row damage since the previous snapshot.
    pub dirty_rows: Vec<bool>,
    /// Whether the program is using its alternate screen.
    pub alternate: bool,
    /// Number of history rows preceding the active screen.
    pub scrollback_rows: usize,
    /// Offset and extent of the emulator viewport used to produce these rows.
    pub viewport: Viewport,
    /// Whether this snapshot establishes a new authoritative sequence baseline.
    pub reset: bool,
    /// Stream bytes this snapshot consumed; return credit only after display consumption.
    pub consumed_bytes: u32,
    /// Original delivery identity; absent for local changes and offline fixtures.
    pub receipt: Option<CreditReceipt>,
    /// Query replies to forward as pane input independently of painting.
    pub responses: Vec<u8>,
}

/// Orders are processed in channel order on the emulator thread.
#[derive(Debug)]
pub enum VtCommand {
    /// Replace terminal state with a server screen; also creates a pane.
    Screen {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// Authoritative byte position after applying the screen.
        sequence: Sequence,
        /// Initial cell width.
        columns: u16,
        /// Initial cell height.
        rows: u16,
        /// Serialized screen, never counted as stream credit.
        bytes: Vec<u8>,
        /// Window theme, retained across program color overrides.
        theme: Box<TerminalTheme>,
    },
    /// Apply contiguous output. A gap poisons the pane until a fresh screen.
    Feed {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// Absolute position of the first byte.
        sequence: Sequence,
        /// Raw terminal output.
        bytes: Vec<u8>,
        /// Original delivery receipt, retained until display consumption.
        receipt: Option<CreditReceipt>,
    },
    /// Apply cell dimensions and publish the resulting snapshot.
    Resize {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// New cell width.
        columns: u16,
        /// New cell height.
        rows: u16,
    },
    /// Change defaults without overwriting the program's OSC overrides.
    Theme {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// New application colors.
        theme: Box<TerminalTheme>,
    },
    /// Move within emulator-owned history and publish its new visible rows.
    Scroll {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// Relative movement or an absolute top/bottom destination.
        scroll: ScrollViewport,
    },
    /// Encode input using live modes without producing another render snapshot.
    Input {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// Owned platform input, paste or pointer event.
        input: TerminalInput,
    },
    /// Publish current state without feeding any bytes.
    Snapshot(PaneKey),
    /// Destroy a pane and its callbacks on their owning thread.
    Close(PaneKey),
}

impl VtCommand {
    /// Identity shared by all commands.
    fn key(&self) -> &PaneKey {
        match self {
            Self::Screen { key, .. }
            | Self::Feed { key, .. }
            | Self::Resize { key, .. }
            | Self::Theme { key, .. }
            | Self::Scroll { key, .. }
            | Self::Input { key, .. }
            | Self::Snapshot(key)
            | Self::Close(key) => key,
        }
    }
}

/// Failure that is surfaced to the caller rather than silently dropping bytes.
#[derive(Debug)]
pub enum VtError {
    /// Runtime or operating-system thread creation failed.
    Thread(std::io::Error),
    /// Emulator rejected an operation.
    Emulator(libghostty_vt::Error),
    /// Input metadata is invalid and was not passed to the native encoder.
    Input(&'static str),
    /// Service has stopped accepting commands.
    Stopped,
    /// Caller must request a fresh screen before continuing this pane.
    NeedsScreen,
    /// Delivery receipt does not match the addressed pane and byte count.
    Credit,
    /// Byte position or per-batch credit does not fit its protocol field.
    Overflow,
}

impl core::fmt::Display for VtError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Thread(source) => write!(formatter, "terminal thread: {source}"),
            Self::Emulator(source) => write!(formatter, "terminal emulator: {source}"),
            Self::Input(reason) => write!(formatter, "terminal input: {reason}"),
            Self::Stopped => formatter.write_str("terminal thread stopped"),
            Self::NeedsScreen => {
                formatter.write_str("terminal sequence gap: request a fresh screen")
            }
            Self::Credit => formatter.write_str("terminal delivery receipt does not match output"),
            Self::Overflow => formatter.write_str("terminal byte count exceeds protocol range"),
        }
    }
}

impl core::error::Error for VtError {}
impl From<libghostty_vt::Error> for VtError {
    fn from(source: libghostty_vt::Error) -> Self {
        Self::Emulator(source)
    }
}

/// An owning-thread result, keeping input latency independent of drawing.
#[derive(Debug)]
pub enum VtOutput {
    /// Plain selected text for the clipboard, never forwarded as pane input.
    Clipboard(String),
    /// Pointer gesture unconsumed by live mouse tracking, returned to its displayed grid.
    LocalPointer(Box<crate::input::PointerInput>),
    /// New owned cell state; credit remains attached until the grid consumes it.
    Snapshot(Box<TerminalSnapshot>),
    /// Mode-encoded bytes to forward immediately through the engine's input path.
    Input(Vec<u8>),
}

/// One command's result. Failures retain identity so the caller can resynchronize.
#[derive(Debug)]
pub struct VtEvent {
    /// Pane the result concerns.
    pub key: PaneKey,
    /// Owned snapshot, encoded input or clipboard text; none for close, or an explicit failure.
    pub result: Result<Option<VtOutput>, VtError>,
}

/// Channel handle and owner of the single emulator thread.
#[derive(Debug)]
pub struct VtThread {
    /// Closing this sender wakes the thread and drops all terminals locally.
    commands: Option<UnboundedSender<VtCommand>>,
    /// Owned render/input results and failures waiting for the application.
    events: Receiver<VtEvent>,
    /// Joined on application shutdown after closing the command channel.
    thread: Option<JoinHandle<()>>,
}

impl VtThread {
    /// Start one current-thread runtime with a `LocalSet` owning all terminals.
    ///
    /// # Errors
    /// Returns `Thread` if runtime or operating-system thread creation fails.
    pub fn start(options: VtOptions) -> Result<Self, VtError> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(VtError::Thread)?;
        let (commands, mut receiving) = unbounded_channel::<VtCommand>();
        let (sending, events) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("iznik-app-vt".to_owned())
            .spawn(move || {
                LocalSet::new().block_on(&runtime, async move {
                    let mut panes = BTreeMap::new();
                    while let Some(command) = receiving.recv().await {
                        let key = command.key().clone();
                        let result = apply(&mut panes, command, &options);
                        if !publish(&sending, key, result) {
                            break;
                        }
                    }
                });
            })
            .map_err(VtError::Thread)?;
        Ok(Self {
            commands: Some(commands),
            events,
            thread: Some(thread),
        })
    }

    /// Queue work without waiting for the emulator or engine.
    ///
    /// # Errors
    /// Returns `Stopped` if the thread is shutting down.
    pub fn send(&self, command: VtCommand) -> Result<(), VtError> {
        self.commands
            .as_ref()
            .ok_or(VtError::Stopped)?
            .send(command)
            .map_err(|_closed| VtError::Stopped)
    }

    /// Poll one result without blocking the window thread.
    #[must_use]
    pub fn poll(&self) -> Option<VtEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for VtThread {
    fn drop(&mut self) {
        drop(self.commands.take());
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// Publish without holding a terminal borrow or callback lock.
fn publish(
    sender: &Sender<VtEvent>,
    key: PaneKey,
    result: Result<Option<VtOutput>, VtError>,
) -> bool {
    sender.send(VtEvent { key, result }).is_ok()
}

/// Terminal state held exclusively on the owning thread.
struct PaneTerminal {
    /// Non-Send emulator and its registered callbacks.
    terminal: Terminal<'static, 'static>,
    /// Reusable input encoders owned by this terminal thread.
    input: InputEncoder,
    /// Reusable emulator damage tracker.
    render: RenderState<'static>,
    /// Answers emitted synchronously while feeding a batch.
    responses: Rc<RefCell<Vec<u8>>>,
    /// Scheme read by the callback when a program asks about theme.
    scheme: Rc<RefCell<ColorScheme>>,
    /// Expected next stream position; absent after any discontinuity.
    sequence: Option<Sequence>,
}

impl PaneTerminal {
    /// Construct callbacks on the owning thread and apply application colors.
    ///
    /// # Errors
    /// Propagates emulator construction, callback, or theme failures.
    fn new(
        columns: u16,
        rows: u16,
        theme: &TerminalTheme,
        options: &VtOptions,
    ) -> Result<Self, VtError> {
        let mut terminal = Terminal::new(Options {
            cols: columns,
            rows,
            max_scrollback: options.scrollback_bytes,
        })?;
        let responses = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&responses);
        terminal
            .on_pty_write(move |_terminal, bytes| sink.borrow_mut().extend_from_slice(bytes))?;
        terminal.on_device_attributes(|_terminal| {
            Some(DeviceAttributes {
                primary: PRIMARY_ATTRIBUTES,
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 0,
                    rom_cartridge: 0,
                },
                tertiary: TertiaryDeviceAttributes { unit_id: 0 },
            })
        })?;
        terminal.on_xtversion(|_terminal| Some(VERSION))?;
        terminal.on_enquiry(|_terminal| Some(""))?;
        terminal.on_size(|terminal| {
            Some(SizeReportSize {
                rows: terminal.rows().ok()?,
                columns: terminal.cols().ok()?,
                cell_width: 0,
                cell_height: 0,
            })
        })?;
        let scheme = Rc::new(RefCell::new(theme.scheme));
        let reading = Rc::clone(&scheme);
        terminal.on_color_scheme(move |_terminal| Some(*reading.borrow()))?;
        let mut pane = Self {
            terminal,
            input: InputEncoder::new(options.maximum_wheel_reports)?,
            render: RenderState::new()?,
            responses,
            scheme,
            sequence: None,
        };
        pane.theme(theme)?;
        Ok(pane)
    }

    /// Change defaults while preserving application-set OSC overrides.
    ///
    /// # Errors
    /// Propagates emulator setter failures.
    fn theme(&mut self, theme: &TerminalTheme) -> Result<(), VtError> {
        self.terminal.set_default_fg_color(Some(theme.foreground))?;
        self.terminal.set_default_bg_color(Some(theme.background))?;
        self.terminal.set_default_cursor_color(theme.cursor)?;
        self.terminal.set_default_color_palette(theme.palette)?;
        *self.scheme.borrow_mut() = theme.scheme;
        Ok(())
    }

    /// Feed only contiguous bytes; a gap requires authoritative replacement.
    ///
    /// # Errors
    /// Returns `NeedsScreen` for a gap, `Overflow` for an invalid byte count,
    /// or an emulator error when its viewport cannot be read.
    fn feed(&mut self, sequence: Sequence, bytes: &[u8]) -> Result<u32, VtError> {
        if self.sequence != Some(sequence) {
            self.sequence = None;
            return Err(VtError::NeedsScreen);
        }
        let length = u64::try_from(bytes.len()).map_err(|_large| VtError::Overflow)?;
        let credit = u32::try_from(bytes.len()).map_err(|_large| VtError::Overflow)?;
        let next = sequence.0.checked_add(length).ok_or(VtError::Overflow)?;
        let follow = self.viewport()?.at_bottom();
        self.terminal.vt_write(bytes);
        if follow {
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
        }
        self.sequence = Some(Sequence(next));
        Ok(credit)
    }

    /// Read the actual emulator viewport, including history eviction.
    ///
    /// # Errors
    /// Propagates an emulator scrollbar read failure.
    fn viewport(&self) -> Result<Viewport, VtError> {
        let scrollbar = self.terminal.scrollbar()?;
        Ok(Viewport {
            total: scrollbar.total,
            offset: scrollbar.offset,
            rows: scrollbar.len,
        })
    }

    /// Encode input only against synchronized modes; copy must match the displayed frame.
    ///
    /// # Errors
    /// Returns a missing authoritative screen, stale selection or encoder error.
    fn input(&mut self, key: &PaneKey, input: &TerminalInput) -> Result<VtOutput, VtError> {
        let sequence = self.sequence.ok_or(VtError::NeedsScreen)?;
        if let TerminalInput::Copy(copy) = input
            && (copy.frame.key != *key
                || copy.frame.sequence != sequence
                || copy.frame.columns != self.terminal.cols()?
                || copy.frame.viewport != self.viewport()?)
        {
            return Err(VtError::Input(
                "selection no longer matches the displayed frame",
            ));
        }
        self.input.encode(&self.terminal, input)
    }

    /// Copy emulator render data into owned, Send values and reset both damage layers.
    ///
    /// # Errors
    /// Propagates emulator reads, or `NeedsScreen` after a gap.
    fn snapshot(&mut self, key: PaneKey, consumed_bytes: u32) -> Result<TerminalSnapshot, VtError> {
        let sequence = self.sequence.ok_or(VtError::NeedsScreen)?;
        let viewport = self.viewport()?;
        let snapshot = self.render.update(&self.terminal)?;
        let colors = snapshot.colors()?;
        let mut rows = Vec::new();
        let mut dirty_rows = Vec::new();
        let mut row_iterator = RowIterator::new()?;
        let mut cell_iterator = CellIterator::new()?;
        let mut iterator = row_iterator.update(&snapshot)?;
        while let Some(row) = iterator.next() {
            dirty_rows.push(row.dirty()?);
            let mut cells = Vec::new();
            let mut reading = cell_iterator.update(row)?;
            while let Some(cell) = reading.next() {
                cells.push(CellSnapshot {
                    text: cell.graphemes()?.into_iter().collect(),
                    width: cell.raw_cell()?.wide()?,
                    style: cell.style()?,
                    foreground: cell.fg_color()?.unwrap_or(colors.foreground),
                    background: cell.bg_color()?.unwrap_or(colors.background),
                });
            }
            row.set_dirty(false)?;
            rows.push(cells);
        }
        let result = TerminalSnapshot {
            key,
            sequence,
            columns: snapshot.cols()?,
            rows,
            cursor: if snapshot.cursor_visible()? {
                snapshot.cursor_viewport()?
            } else {
                None
            },
            cursor_style: snapshot.cursor_visual_style()?,
            colors,
            dirty: snapshot.dirty()?,
            dirty_rows,
            alternate: self.terminal.active_screen()? == Screen::Alternate,
            scrollback_rows: self.terminal.scrollback_rows()?,
            viewport,
            reset: false,
            consumed_bytes,
            receipt: None,
            responses: std::mem::take(&mut *self.responses.borrow_mut()),
        };
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(result)
    }
}

/// Apply one ordered command and produce a complete render snapshot.
///
/// # Errors
/// Reports missing/gapped panes, invalid counts, and emulator failures.
fn apply(
    panes: &mut BTreeMap<PaneKey, PaneTerminal>,
    command: VtCommand,
    options: &VtOptions,
) -> Result<Option<VtOutput>, VtError> {
    let key = command.key().clone();
    let reset = matches!(&command, VtCommand::Screen { .. });
    let receipt = delivery_receipt(&command)?;
    let consumed_bytes = match command {
        VtCommand::Screen {
            sequence,
            columns,
            rows,
            bytes,
            theme,
            ..
        } => {
            let mut pane = PaneTerminal::new(columns, rows, &theme, options)?;
            pane.terminal.vt_write(&bytes);
            // Reconstructed historical state must never replay query effects.
            pane.responses.borrow_mut().clear();
            pane.sequence = Some(sequence);
            panes.insert(key.clone(), pane);
            0
        }
        VtCommand::Feed {
            sequence, bytes, ..
        } => panes
            .get_mut(&key)
            .ok_or(VtError::NeedsScreen)?
            .feed(sequence, &bytes)?,
        VtCommand::Resize { columns, rows, .. } => {
            panes
                .get_mut(&key)
                .ok_or(VtError::NeedsScreen)?
                .terminal
                .resize(columns, rows, 0, 0)?;
            0
        }
        VtCommand::Theme { theme, .. } => {
            panes
                .get_mut(&key)
                .ok_or(VtError::NeedsScreen)?
                .theme(&theme)?;
            0
        }
        VtCommand::Scroll { scroll, .. } => {
            panes
                .get_mut(&key)
                .ok_or(VtError::NeedsScreen)?
                .terminal
                .scroll_viewport(scroll);
            0
        }
        VtCommand::Input { input, .. } => {
            let pane = panes.get_mut(&key).ok_or(VtError::NeedsScreen)?;
            return pane.input(&key, &input).map(Some);
        }
        VtCommand::Snapshot(_) => 0,
        VtCommand::Close(_) => {
            panes.remove(&key);
            return Ok(None);
        }
    };
    panes
        .get_mut(&key)
        .ok_or(VtError::NeedsScreen)?
        .snapshot(key, consumed_bytes)
        .map(|mut snapshot| {
            snapshot.reset = reset;
            snapshot.receipt = receipt;
            Some(VtOutput::Snapshot(Box::new(snapshot)))
        })
}

/// Check delivery metadata before any bytes reach the native terminal.
///
/// # Errors
/// Returns `Credit` when a receipt describes another pane or byte count.
fn delivery_receipt(command: &VtCommand) -> Result<Option<CreditReceipt>, VtError> {
    let VtCommand::Feed {
        key,
        bytes,
        receipt: Some(receipt),
        ..
    } = command
    else {
        return Ok(None);
    };
    if receipt.host() != &key.host
        || receipt.pane() != key.pane
        || usize::try_from(receipt.bytes()).ok() != Some(bytes.len())
    {
        return Err(VtError::Credit);
    }
    Ok(Some(receipt.clone()))
}
