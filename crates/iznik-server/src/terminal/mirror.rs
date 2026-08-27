//! The `libghostty-vt` terminal every pane's bytes are fed into — the engine the
//! client renders with — and the one thread every mirror lives on.
//!
//! Because the emulator handle is `!Send`, each pane's mirror and its async task
//! live on a single OS thread running a current-thread runtime and a
//! `tokio::task::LocalSet`; [`MirrorThread`] owns that thread and accepts a
//! `Send` constructor per pane, building the `!Send` future there.
//!
//! **How libghostty-vt 0.2.1 routes a query (confirmed empirically, this crate
//! version):** every response the terminal emits toward the pseudoterminal
//! leaves through `on_pty_write` — the single collection point. A query the
//! terminal answers from its own state (cursor-position report `CSI 6 n`, device
//! status `CSI 5 n`) is written straight through it. A query only the embedder
//! can answer reaches its own callback — device attributes (`CSI c` / `> c` /
//! `= c`), `XTVERSION` (`CSI > q`), the answerback (`ENQ`), the text-area size
//! (`CSI 14 t` / `CSI 18 t`), the colour scheme (`CSI ? 996 n`) — which returns
//! a value the terminal then formats and writes through `on_pty_write`. So the
//! mirror collects the pending buffer from `on_pty_write` alone, and the answer
//! callbacks only supply iznik's fixed values — of which the empty answerback is
//! the one that writes nothing back (a callback returning `None` would too, but
//! the mirror always answers with a value).

use std::cell::RefCell;
use std::fmt;
use std::future::Future;
use std::rc::Rc;
use std::thread::{self, JoinHandle};

use libghostty_vt::Error as EmulatorError;
use libghostty_vt::screen::Screen;
use libghostty_vt::terminal::{
    ColorScheme, ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Options,
    PrimaryDeviceAttributes, SecondaryDeviceAttributes, SizeReportSize, Terminal,
    TertiaryDeviceAttributes,
};
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::mpsc;
use tokio::task::LocalSet;

/// The scrollback budget the mirror keeps. The architecture names it in lines,
/// but libghostty-vt 0.2.1 spends `max_scrollback` as a memory budget rather than
/// a line count (confirmed empirically, this crate version), so the rows it
/// actually retains are fewer than this and vary with line width; either way the
/// scrollback is bounded, and the mirror keeps the value the architecture sets.
pub const MIRROR_SCROLLBACK_ROWS: usize = 10_000;

/// The XTVERSION the mirror answers: the crate name and version.
const XTVERSION: &str = concat!("iznik-server ", env!("CARGO_PKG_VERSION"));

/// The answerback the mirror gives to `ENQ`: none.
const ANSWERBACK: &str = "";

/// The conformance level the mirror reports in its primary device attributes.
const PRIMARY_CONFORMANCE: ConformanceLevel = ConformanceLevel::VT220;

/// The features the mirror reports in its primary device attributes: none, the
/// widely compatible base; a real client answers with its own when attached.
const PRIMARY_FEATURES: &[DeviceAttributeFeature] = &[];

/// The device type the mirror reports in its secondary device attributes.
const SECONDARY_DEVICE_TYPE: DeviceType = DeviceType::VT220;

/// The firmware version the mirror reports in its secondary device attributes.
const SECONDARY_FIRMWARE: u16 = 0;

/// The ROM cartridge number: always zero for an emulator.
const SECONDARY_ROM_CARTRIDGE: u16 = 0;

/// The unit id the mirror reports in its tertiary device attributes.
const TERTIARY_UNIT_ID: u32 = 0;

/// A cell has no pixel size the mirror knows or reports.
const CELL_PIXELS: u32 = 0;

/// Anything that went wrong building a mirror or its thread.
#[derive(Debug)]
pub enum MirrorError {
    /// The emulator could not be created or a callback could not be registered.
    Emulator {
        /// What the emulator reported.
        source: EmulatorError,
    },
    /// The mirror thread could not be spawned.
    Thread {
        /// What the operating system reported.
        source: std::io::Error,
    },
}

impl fmt::Display for MirrorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirrorError::Emulator { source } => {
                write!(formatter, "the emulator failed: {source:?}")
            }
            MirrorError::Thread { source } => {
                write!(formatter, "the mirror thread failed: {source}")
            }
        }
    }
}

impl std::error::Error for MirrorError {}

/// The bytes the terminal wants written to the pseudoterminal, held until the
/// pane writes them, and how many subscribers are attached.
#[derive(Debug, Default)]
struct Pending {
    /// How many clients are subscribed to the pane.
    subscribers: usize,
    /// The bytes collected from `on_pty_write` while nobody is subscribed.
    bytes: Vec<u8>,
}

/// One pane's emulator: fed every byte, queried for what the pane looks like,
/// and the source of the responses a pane writes back when nobody is attached.
pub struct Mirror {
    /// The emulator, and the callbacks it holds, which share [`Mirror::pending`].
    terminal: Terminal<'static, 'static>,
    /// The pending responses and subscriber count, shared with the callbacks.
    pending: Rc<RefCell<Pending>>,
    /// The current column count.
    columns: u16,
    /// The current row count.
    rows: u16,
}

impl fmt::Debug for Mirror {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Mirror")
            .field("columns", &self.columns)
            .field("rows", &self.rows)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Mirror {
    /// A mirror sized `columns` by `rows`, with the callbacks and response policy
    /// registered.
    ///
    /// # Errors
    ///
    /// [`MirrorError::Emulator`] when the emulator cannot be created or a
    /// callback cannot be registered.
    pub fn new(columns: u16, rows: u16) -> Result<Mirror, MirrorError> {
        let pending = Rc::new(RefCell::new(Pending::default()));
        let mut terminal = Terminal::new(Options {
            cols: columns,
            rows,
            max_scrollback: MIRROR_SCROLLBACK_ROWS,
        })
        .map_err(|source| MirrorError::Emulator { source })?;

        let sink = Rc::clone(&pending);
        terminal
            .on_pty_write(move |_terminal, data| {
                let mut state = sink.borrow_mut();
                if state.subscribers == 0 {
                    state.bytes.extend_from_slice(data);
                }
            })
            .map_err(|source| MirrorError::Emulator { source })?;
        terminal
            .on_device_attributes(|_terminal| Some(device_attributes()))
            .map_err(|source| MirrorError::Emulator { source })?;
        terminal
            .on_xtversion(|_terminal| Some(XTVERSION))
            .map_err(|source| MirrorError::Emulator { source })?;
        terminal
            .on_enquiry(|_terminal| Some(ANSWERBACK))
            .map_err(|source| MirrorError::Emulator { source })?;
        terminal
            .on_color_scheme(|_terminal| Some(ColorScheme::Dark))
            .map_err(|source| MirrorError::Emulator { source })?;
        terminal
            .on_size(|terminal| {
                Some(SizeReportSize {
                    rows: terminal.rows().ok()?,
                    columns: terminal.cols().ok()?,
                    cell_width: CELL_PIXELS,
                    cell_height: CELL_PIXELS,
                })
            })
            .map_err(|source| MirrorError::Emulator { source })?;

        Ok(Mirror {
            terminal,
            pending,
            columns,
            rows,
        })
    }

    /// Feeds bytes into the emulator, exactly as the pane received them.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Resizes the emulator; the size it reports and its grid follow.
    pub fn resize(&mut self, columns: u16, rows: u16) {
        if self
            .terminal
            .resize(columns, rows, CELL_PIXELS, CELL_PIXELS)
            .is_ok()
        {
            self.columns = columns;
            self.rows = rows;
        }
    }

    /// The current column count, read from the emulator so a program that
    /// resizes it itself — `DECCOLM` — is not missed; the tracked value is only
    /// the fallback when the emulator cannot report.
    #[must_use]
    pub fn columns(&self) -> u16 {
        self.terminal.cols().unwrap_or(self.columns)
    }

    /// The current row count, read from the emulator, with the tracked value the
    /// fallback.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.terminal.rows().unwrap_or(self.rows)
    }

    /// How many rows have scrolled off the top into scrollback, bounded by the
    /// emulator's scrollback budget (see [`MIRROR_SCROLLBACK_ROWS`]).
    #[must_use]
    pub fn scrollback_rows(&self) -> usize {
        self.terminal.scrollback_rows().unwrap_or(0)
    }

    /// The window title the emulator last saw, empty when none was set.
    #[must_use]
    pub fn title(&self) -> String {
        self.terminal.title().map(str::to_owned).unwrap_or_default()
    }

    /// The working directory the emulator last saw, as its `file://` URL, or none
    /// when none was reported.
    #[must_use]
    pub fn working_directory(&self) -> Option<String> {
        self.terminal
            .pwd()
            .ok()
            .filter(|pwd| !pwd.is_empty())
            .map(str::to_owned)
    }

    /// Whether the alternate screen is the active one.
    #[must_use]
    pub fn in_alternate_screen(&self) -> bool {
        self.terminal
            .active_screen()
            .is_ok_and(|screen| screen == Screen::Alternate)
    }

    /// The emulator itself, for the screen serializer to format its state.
    pub(crate) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.terminal
    }

    /// Records how many clients are subscribed. With any subscriber, the pending
    /// responses are discarded — the attached client's own emulator answers.
    pub fn set_subscriber_count(&mut self, count: usize) {
        let mut pending = self.pending.borrow_mut();
        pending.subscribers = count;
        if count > 0 {
            pending.bytes.clear();
        }
    }

    /// Takes the responses collected while nobody was subscribed, for the pane to
    /// write to the pseudoterminal.
    pub fn take_pending_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending.borrow_mut().bytes)
    }
}

/// The device attributes the mirror answers with — a fixed, widely compatible
/// VT220, since a real client answers with its own once attached.
fn device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(PRIMARY_CONFORMANCE, PRIMARY_FEATURES),
        secondary: SecondaryDeviceAttributes {
            device_type: SECONDARY_DEVICE_TYPE,
            firmware_version: SECONDARY_FIRMWARE,
            rom_cartridge: SECONDARY_ROM_CARTRIDGE,
        },
        tertiary: TertiaryDeviceAttributes {
            unit_id: TERTIARY_UNIT_ID,
        },
    }
}

/// A constructor shipped to the mirror thread: run there, it builds a pane's
/// `!Send` future and spawns it on the thread's `LocalSet`.
type Constructor = Box<dyn FnOnce() + Send>;

/// The one OS thread every mirror and pane task lives on: a current-thread
/// runtime and a `LocalSet`, fed `!Send` futures built from `Send` constructors.
#[derive(Debug)]
pub struct MirrorThread {
    /// Constructors sent to the thread; dropping it ends the thread.
    constructors: Option<mpsc::UnboundedSender<Constructor>>,
    /// The thread, joined on drop.
    handle: Option<JoinHandle<()>>,
}

impl MirrorThread {
    /// Starts the mirror thread, once its runtime is built and ready.
    ///
    /// # Errors
    ///
    /// [`MirrorError::Thread`] when the operating system cannot spawn the thread,
    /// or the thread's runtime cannot be built, or it ends before it is ready.
    pub fn start() -> Result<MirrorThread, MirrorError> {
        let (constructors, receiver) = mpsc::unbounded_channel();
        let (ready, readiness) = std::sync::mpsc::channel();
        let handle = thread::Builder::new()
            .name("iznik-mirror".to_owned())
            .spawn(move || run(receiver, &ready))
            .map_err(|source| MirrorError::Thread { source })?;
        match readiness.recv() {
            Ok(Ok(())) => Ok(MirrorThread {
                constructors: Some(constructors),
                handle: Some(handle),
            }),
            Ok(Err(source)) => {
                let _joined = handle.join();
                Err(MirrorError::Thread { source })
            }
            Err(_disconnected) => {
                let _joined = handle.join();
                Err(MirrorError::Thread {
                    source: std::io::Error::other("the mirror thread ended before it was ready"),
                })
            }
        }
    }

    /// Ships a constructor to the thread, which builds its `!Send` future there
    /// and spawns it locally. The pane's emulator never leaves the thread.
    pub fn spawn<F>(&self, build: impl FnOnce() -> F + Send + 'static)
    where
        F: Future<Output = ()> + 'static,
    {
        let constructor: Constructor = Box::new(move || {
            let _task = tokio::task::spawn_local(build());
        });
        if let Some(constructors) = self.constructors.as_ref() {
            let _sent = constructors.send(constructor);
        }
    }
}

impl Drop for MirrorThread {
    fn drop(&mut self) {
        // Dropping the sender ends the thread's receive loop, which returns from
        // `block_on` and drops the `LocalSet` and every task on it.
        self.constructors = None;
        if let Some(handle) = self.handle.take() {
            let _joined = handle.join();
        }
    }
}

/// The mirror thread's body: build the runtime, report whether it built, then a
/// `LocalSet` drains constructors, each building a pane's future and spawning it
/// here, until the sender is dropped.
fn run(
    mut receiver: mpsc::UnboundedReceiver<Constructor>,
    ready: &std::sync::mpsc::Sender<Result<(), std::io::Error>>,
) {
    let runtime = match RuntimeBuilder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _sent = ready.send(Err(error));
            return;
        }
    };
    let _sent = ready.send(Ok(()));
    let local = LocalSet::new();
    local.block_on(&runtime, async move {
        while let Some(constructor) = receiver.recv().await {
            constructor();
        }
    });
}
