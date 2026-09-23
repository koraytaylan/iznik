//! One `LocalSet` thread owns every application emulator. Only owned snapshots
//! and commands cross its channel; no terminal handle leaves the thread.

mod batch;
mod eviction;

use crate::input::{InputEncoder, TerminalInput};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::answered_length;
use iznik_client::host::manager::credit::CreditReceipt;
use iznik_protocol::identity::{PaneId, Sequence};
use libghostty_vt::render::{
    CellIterator, Colors, CursorViewport, CursorVisualStyle, Dirty, RenderState, RowIterator,
};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::style::{Palette, RgbColor, Style};
use libghostty_vt::terminal::{
    ColorScheme, ConformanceLevel, DeviceAttributes, DeviceType, Options, Point, PointCoordinate,
    PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes, SizeReportSize, Terminal,
    TertiaryDeviceAttributes,
};

use crate::clipboard::ClipboardScan;
use crate::wake::WakeSignal;
use tokio::runtime::Builder;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;

/// Default emulator history memory budget per pane: 10 MiB, which holds
/// tens of thousands of rows of ordinary output. The `scrollback_bytes`
/// setting replaces it.
pub const SCROLLBACK_BYTES: usize = 10_485_760;
/// Least history budget a setting may ask for: 64 KiB, a few hundred rows.
pub const MINIMUM_SCROLLBACK_BYTES: usize = 65_536;
/// Most history budget a setting may ask for: 1 GiB per pane.
pub const MAXIMUM_SCROLLBACK_BYTES: usize = 1_073_741_824;
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
            scheme: scheme_for(BACKGROUND),
        }
    }
}

/// Weight of red in relative luminance (ITU-R BT.709).
const RED_LUMINANCE: f32 = 0.2126;
/// Weight of green in relative luminance.
const GREEN_LUMINANCE: f32 = 0.7152;
/// Weight of blue in relative luminance.
const BLUE_LUMINANCE: f32 = 0.0722;
/// The largest channel value, which scales a byte to a fraction.
const CHANNEL_MAXIMUM: f32 = 255.0;
/// Relative luminance above which a background reads as light.
const LIGHT_THRESHOLD: f32 = 0.5;

/// The scheme a program is told when it asks whether the terminal is light
/// or dark: light when the background's luminance is over half.
#[must_use]
pub fn scheme_for(background: RgbColor) -> ColorScheme {
    let luminance = (RED_LUMINANCE * f32::from(background.r)
        + GREEN_LUMINANCE * f32::from(background.g)
        + BLUE_LUMINANCE * f32::from(background.b))
        / CHANNEL_MAXIMUM;
    if luminance > LIGHT_THRESHOLD {
        ColorScheme::Light
    } else {
        ColorScheme::Dark
    }
}

/// Resource limits for the thread, adjustable by a test.
#[derive(Clone, Debug)]
pub struct VtOptions {
    /// Per-pane history memory budget, in bytes rather than rows.
    pub scrollback_bytes: usize,
    /// Maximum native wheel reports emitted by one platform request; excess is refused.
    pub maximum_wheel_reports: usize,
    /// Most queued commands taken together, so a pane's queued output is fed
    /// before one snapshot rather than one snapshot per chunk.
    pub maximum_batch: usize,
}

/// A wheel burst stays bounded to a few kilobytes and cannot monopolize the VT owner.
const MAXIMUM_WHEEL_REPORTS: usize = 128;
/// Enough queued chunks to absorb a flood in one snapshot, few enough that a
/// keystroke's reply is never held behind more than a moment of output.
const MAXIMUM_BATCH: usize = 256;

impl Default for VtOptions {
    fn default() -> Self {
        Self {
            scrollback_bytes: SCROLLBACK_BYTES,
            maximum_wheel_reports: MAXIMUM_WHEEL_REPORTS,
            maximum_batch: MAXIMUM_BATCH,
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
    /// OSC 8 target for this cell. The visible grapheme is only the label.
    pub link: Option<String>,
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
    /// Rows the emulator has let go of from the top of its history since it
    /// began, so that `evicted + offset` numbers a row the same way however
    /// many are let go after it.
    pub evicted: u64,
}

impl Viewport {
    /// Whether incoming output should keep the viewport at the active screen.
    #[must_use]
    pub fn at_bottom(self) -> bool {
        // Saturating arithmetic also treats an out-of-range offset as bottom.
        self.offset.saturating_add(self.rows) >= self.total
    }

    /// The top row shown, counted from the first row the emulator ever held
    /// rather than from the oldest it still holds.
    #[must_use]
    pub fn top(self) -> u64 {
        self.evicted.saturating_add(self.offset)
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
    /// A row the emulator did not change is shared with the previous snapshot.
    pub rows: Vec<Arc<[CellSnapshot]>>,
    /// Visible cursor position, absent when outside the viewport or hidden.
    pub cursor: Option<CursorViewport>,
    /// Block, underline, or bar requested by the program.
    pub cursor_style: CursorVisualStyle,
    /// Active default colors and palette.
    pub colors: Colors,
    /// Damage state since the previous snapshot.
    pub dirty: Dirty,
    /// Per-row damage since the previous snapshot: true where the row's cells
    /// were read again, false where the previous snapshot's row is reused.
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
    /// Original delivery identities of every chunk fed since the previous
    /// snapshot, in order; empty for local changes and offline fixtures.
    pub receipts: Vec<CreditReceipt>,
    /// Query replies to forward as pane input independently of painting.
    pub responses: Vec<u8>,
    /// Plain text a program copied, in arrival order. Empty after a reconstructed screen.
    pub clipboard: Vec<String>,
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
    /// Say how far the host already answered this pane's terminal queries:
    /// the responses fed bytes before `through` produce are not sent again.
    Answered {
        /// Host-qualified pane identity.
        key: PaneKey,
        /// The stream position up to which queries were answered.
        through: Sequence,
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
            | Self::Answered { key, .. }
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
    /// Pasted text with a line break for a program without bracketed paste:
    /// nothing was sent, and a person must confirm it first.
    MultilinePaste(String),
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
    /// Raised after every result the thread publishes.
    signal: WakeSignal,
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
        let signal = WakeSignal::new();
        let raising = signal.clone();
        let thread = thread::Builder::new()
            .name("iznik-app-vt".to_owned())
            .spawn(move || {
                LocalSet::new().block_on(&runtime, async move {
                    let mut owner = batch::Owner::new(options);
                    while let Some(command) = receiving.recv().await {
                        let mut results = Vec::new();
                        owner.take(command, &mut results);
                        for _taken in 1..owner.maximum_batch() {
                            let Ok(next) = receiving.try_recv() else {
                                break;
                            };
                            owner.take(next, &mut results);
                        }
                        owner.finish(&mut results);
                        let open = results
                            .into_iter()
                            .all(|(key, result)| publish(&sending, key, result));
                        raising.raise();
                        if !open {
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
            signal,
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

    /// The signal raised after every result the thread publishes.
    #[must_use]
    pub fn wake_signal(&self) -> WakeSignal {
        self.signal.clone()
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
    /// Program clipboard text emitted synchronously while feeding a batch.
    clipboard: Rc<RefCell<Vec<String>>>,
    /// Partial OSC 52 carried across output batches.
    scan: ClipboardScan,
    /// Scheme read by the callback when a program asks about theme.
    scheme: Rc<RefCell<ColorScheme>>,
    /// Expected next stream position; absent after any discontinuity.
    sequence: Option<Sequence>,
    /// Rows of the last snapshot, reused for rows the emulator left clean.
    published: Vec<Arc<[CellSnapshot]>>,
    /// Viewport of the last snapshot; a different one reads every row again.
    published_viewport: Option<Viewport>,
    /// How many rows the history has let go of, for numbering that holds.
    eviction: eviction::Eviction,
    /// Where the host's own answers to terminal queries end: responses to
    /// bytes before it were already sent by the host and are not sent again.
    answered: Sequence,
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
            clipboard: Rc::new(RefCell::new(Vec::new())),
            scan: ClipboardScan::new(),
            scheme,
            sequence: None,
            published: Vec::new(),
            published_viewport: None,
            eviction: eviction::Eviction::default(),
            answered: Sequence(0),
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
        self.scan.observe(bytes, &mut self.clipboard.borrow_mut());
        let answered = answered_length(sequence, self.answered, bytes.len());
        let (replayed, fresh) = bytes.split_at_checked(answered).unwrap_or((bytes, &[]));
        // The host answered the queries in these bytes while nobody was
        // attached; answering them again would type stray replies.
        let kept = self.responses.borrow().len();
        self.terminal.vt_write(replayed);
        self.responses.borrow_mut().truncate(kept);
        self.terminal.vt_write(fresh);
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
            evicted: self.eviction.evicted(),
        })
    }

    /// Encode input only against synchronized modes; copy must match the displayed frame.
    ///
    /// # Errors
    /// Returns a missing authoritative screen, stale selection or encoder error.
    fn input(&mut self, key: &PaneKey, input: &TerminalInput) -> Result<VtOutput, VtError> {
        self.sequence.ok_or(VtError::NeedsScreen)?;
        // A copy names rows counted from the first the emulator ever held,
        // which neither output nor a full history letting rows go moves:
        // only another pane or a width change makes it describe other text,
        // and rows that have been let go cannot be copied at all.
        if let TerminalInput::Copy(copy) = input {
            if copy.frame.key != *key || copy.frame.columns != self.terminal.cols()? {
                return Err(VtError::Input(
                    "selection no longer matches the displayed frame",
                ));
            }
            let evicted = self.eviction.observe(&mut self.terminal)?;
            let mut held = copy.clone();
            held.frame.viewport.offset = copy
                .frame
                .viewport
                .offset
                .checked_sub(evicted)
                .ok_or(VtError::Input("the selection's rows have left the history"))?;
            return self
                .input
                .encode(&self.terminal, &TerminalInput::Copy(held));
        }
        self.input.encode(&self.terminal, input)
    }

    /// Copy emulator render data into owned, Send values and reset both damage layers.
    ///
    /// # Errors
    /// Propagates emulator reads, or `NeedsScreen` after a gap.
    fn snapshot(&mut self, key: PaneKey, consumed_bytes: u32) -> Result<TerminalSnapshot, VtError> {
        let sequence = self.sequence.ok_or(VtError::NeedsScreen)?;
        let _evicted = self.eviction.observe(&mut self.terminal)?;
        let viewport = self.viewport()?;
        let snapshot = self.render.update(&self.terminal)?;
        let colors = snapshot.colors()?;
        let columns = snapshot.cols()?;
        let every_row = snapshot.dirty()? == Dirty::Full
            || self.published_viewport != Some(viewport)
            || self.published.first().map(|row| row.len()) != Some(usize::from(columns));
        let mut rows = Vec::new();
        let mut dirty_rows = Vec::new();
        let mut row_iterator = RowIterator::new()?;
        let mut cell_iterator = CellIterator::new()?;
        let mut iterator = row_iterator.update(&snapshot)?;
        while let Some(row) = iterator.next() {
            let kept = if every_row || row.dirty()? {
                None
            } else {
                self.published.get(rows.len()).cloned()
            };
            dirty_rows.push(kept.is_none());
            let cells = if let Some(cells) = kept {
                cells
            } else {
                let row_index = u32::try_from(rows.len()).map_err(|_row| VtError::Overflow)?;
                let mut cells = Vec::new();
                let mut reading = cell_iterator.update(row)?;
                while let Some(cell) = reading.next() {
                    let column = u16::try_from(cells.len()).map_err(|_column| VtError::Overflow)?;
                    let raw = cell.raw_cell()?;
                    cells.push(CellSnapshot {
                        text: cell.graphemes()?.into_iter().collect(),
                        width: raw.wide()?,
                        style: cell.style()?,
                        foreground: cell.fg_color()?.unwrap_or(colors.foreground),
                        background: cell.bg_color()?.unwrap_or(colors.background),
                        link: cell_link(&self.terminal, column, row_index, raw.has_hyperlink()?)?,
                    });
                }
                Arc::from(cells)
            };
            row.set_dirty(false)?;
            rows.push(cells);
        }
        self.published.clone_from(&rows);
        self.published_viewport = Some(viewport);
        let result = TerminalSnapshot {
            key,
            sequence,
            columns,
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
            receipts: Vec::new(),
            responses: std::mem::take(&mut *self.responses.borrow_mut()),
            clipboard: std::mem::take(&mut *self.clipboard.borrow_mut()),
        };
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(result)
    }
}

/// Read the OSC 8 target of one viewport cell.
///
/// A cell without a hyperlink, or one whose target is not text, yields `None`.
/// The lookup uses viewport coordinates, so a scrolled history row keeps the
/// target that was painted on it.
///
/// # Errors
/// Propagates an emulator grid or hyperlink read failure.
fn cell_link(
    terminal: &Terminal<'static, 'static>,
    column: u16,
    row: u32,
    linked: bool,
) -> Result<Option<String>, VtError> {
    if !linked {
        return Ok(None);
    }
    let reference = terminal.grid_ref(Point::Viewport(PointCoordinate { x: column, y: row }))?;
    let mut buffer = Vec::new();
    loop {
        match reference.hyperlink_uri(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(length) => {
                buffer.truncate(length);
                return Ok(String::from_utf8(buffer)
                    .ok()
                    .filter(|text| !text.is_empty()));
            }
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                if required <= buffer.len() {
                    return Ok(None);
                }
                buffer.resize(required, 0);
            }
            Err(error) => return Err(VtError::Emulator(error)),
        }
    }
}

/// Apply one ordered command and produce a complete render snapshot.
///
/// # Errors
/// Reports missing/gapped panes, invalid counts, and emulator failures.
fn apply(
    panes: &mut BTreeMap<PaneKey, PaneTerminal>,
    pending_size: &mut BTreeMap<PaneKey, (u16, u16)>,
    command: VtCommand,
    options: &VtOptions,
) -> Result<Option<VtOutput>, VtError> {
    let key = command.key().clone();
    let reset = matches!(&command, VtCommand::Screen { .. });
    let receipt = match &command {
        VtCommand::Feed {
            key: fed,
            bytes,
            receipt: delivery,
            ..
        } => delivery_receipt(fed, bytes, delivery.as_ref())?,
        _ => None,
    };
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
            // Reconstructed historical state must never replay query effects or clipboard writes.
            pane.responses.borrow_mut().clear();
            pane.clipboard.borrow_mut().clear();
            pane.sequence = Some(sequence);
            if let Some((pending_columns, pending_rows)) = pending_size.remove(&key)
                && (pending_columns != columns || pending_rows != rows)
            {
                pane.terminal.resize(pending_columns, pending_rows, 0, 0)?;
            }
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
            let Some(pane) = panes.get_mut(&key) else {
                pending_size.insert(key, (columns, rows));
                return Ok(None);
            };
            pane.terminal.resize(columns, rows, 0, 0)?;
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
        VtCommand::Answered { through, .. } => {
            if let Some(pane) = panes.get_mut(&key) {
                pane.answered = pane.answered.max(through);
            }
            return Ok(None);
        }
        VtCommand::Snapshot(_) => 0,
        VtCommand::Close(_) => {
            panes.remove(&key);
            pending_size.remove(&key);
            return Ok(None);
        }
    };
    panes
        .get_mut(&key)
        .ok_or(VtError::NeedsScreen)?
        .snapshot(key, consumed_bytes)
        .map(|mut snapshot| {
            snapshot.reset = reset;
            snapshot.receipts = receipt.into_iter().collect();
            Some(VtOutput::Snapshot(Box::new(snapshot)))
        })
}

/// Check delivery metadata before any bytes reach the native terminal.
///
/// # Errors
/// Returns `Credit` when a receipt describes another pane or byte count.
fn delivery_receipt(
    key: &PaneKey,
    bytes: &[u8],
    receipt: Option<&CreditReceipt>,
) -> Result<Option<CreditReceipt>, VtError> {
    let Some(receipt) = receipt else {
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
