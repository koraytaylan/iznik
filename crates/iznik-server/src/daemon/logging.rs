//! A size-capped rotating log file, because a full disk is worse than no logs.
//!
//! `tracing-appender` rotates by time, which on a machine nobody is watching
//! is a promise about how *often* the disk fills rather than whether it does.
//! This rotates by size and keeps one predecessor, so what a remote daemon can
//! ever occupy is twice the cap and not one byte more.
//!
//! It writes through the runtime rather than through `std::io`, because this
//! server is asynchronous end to end and a log line is not a reason to block a
//! worker thread. A layer formats each event into a line and hands it to one
//! task that owns the file; the channel is the only thing between them.

use std::path::{Path, PathBuf};

use core::fmt::{self, Display, Formatter, Write as _};

use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// The most one log file may hold before it is rotated.
pub const MAXIMUM_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// The environment variable the level is read from.
pub const LEVEL_VARIABLE: &str = "IZNIK_LOG";

/// The level a daemon logs at when nothing says otherwise.
const DEFAULT_LEVEL: &str = "info";

/// The suffix the one kept predecessor is renamed to.
const PREDECESSOR: &str = "1";

/// The field a `tracing` event's message arrives under.
const MESSAGE_FIELD: &str = "message";

/// Why logging could not be set up.
#[derive(Debug)]
pub enum LoggingError {
    /// The log file could not be opened, written or rotated.
    Io {
        /// The log file.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// A subscriber was already installed, which only a second call does.
    AlreadyInstalled,
}

impl Display for LoggingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LoggingError::Io { path, source } => {
                write!(formatter, "the log file {}: {source}", path.display())
            }
            LoggingError::AlreadyInstalled => {
                write!(formatter, "logging was already installed")
            }
        }
    }
}

impl core::error::Error for LoggingError {}

/// The log file and how much of the cap it has spent.
#[derive(Debug)]
pub struct Sink {
    /// Where the log lives.
    path: PathBuf,
    /// The most it may hold before rotating.
    cap: u64,
    /// The open file.
    file: File,
    /// How many bytes it holds.
    bytes: u64,
}

impl Sink {
    /// Opens the log at `path`, capped at `cap` bytes.
    ///
    /// # Errors
    ///
    /// [`LoggingError::Io`] when it cannot be opened or measured.
    pub async fn open(path: &Path, cap: u64) -> Result<Sink, LoggingError> {
        let file = append(path).await?;
        let bytes = file
            .metadata()
            .await
            .map(|held| held.len())
            .unwrap_or_default();
        Ok(Sink {
            path: path.to_path_buf(),
            cap,
            file,
            bytes,
        })
    }

    /// Appends one line, rotating first when the cap has been reached.
    ///
    /// # Errors
    ///
    /// [`LoggingError::Io`] when the file cannot be written or rotated.
    pub async fn write(&mut self, line: &str) -> Result<(), LoggingError> {
        if self.bytes >= self.cap {
            self.rotate().await?;
        }
        let refused = |source| LoggingError::Io {
            path: self.path.clone(),
            source,
        };
        self.file
            .write_all(line.as_bytes())
            .await
            .map_err(refused)?;
        self.file.flush().await.map_err(refused)?;
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(line.len()).unwrap_or(0));
        Ok(())
    }

    /// Renames the log over its one predecessor and opens a fresh one.
    ///
    /// # Errors
    ///
    /// [`LoggingError::Io`] when it cannot be renamed or reopened.
    async fn rotate(&mut self) -> Result<(), LoggingError> {
        let refused = |source| LoggingError::Io {
            path: self.path.clone(),
            source,
        };
        tokio::fs::rename(&self.path, predecessor(&self.path))
            .await
            .map_err(refused)?;
        self.file = append(&self.path).await?;
        self.bytes = 0;
        Ok(())
    }
}

/// The name the one kept predecessor is renamed to.
#[must_use]
pub fn predecessor(path: &Path) -> PathBuf {
    let mut older = path.to_path_buf().into_os_string();
    older.push(".");
    older.push(PREDECESSOR);
    PathBuf::from(older)
}

/// Opens a log file for appending, creating it if it is not there.
///
/// # Errors
///
/// [`LoggingError::Io`] when it cannot be opened.
async fn append(path: &Path) -> Result<File, LoggingError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|source| LoggingError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// The words of one event, gathered as it is visited.
#[derive(Debug, Default)]
struct Words {
    /// What the event said.
    said: String,
}

impl Visit for Words {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let separator = if self.said.is_empty() { "" } else { " " };
        // A line that will not format is a line that is not logged; there is
        // nowhere left to report it to.
        if field.name() == MESSAGE_FIELD {
            let _recorded = write!(self.said, "{separator}{value:?}");
        } else {
            let _recorded = write!(self.said, "{separator}{}={value:?}", field.name());
        }
    }
}

/// The layer that turns each event into a line and hands it to the task that
/// owns the file.
#[derive(Debug)]
struct Lines {
    /// Where lines go.
    lines: mpsc::UnboundedSender<String>,
}

impl<Collector: tracing::Subscriber> Layer<Collector> for Lines {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, Collector>,
    ) {
        let mut words = Words::default();
        event.record(&mut words);
        let metadata = event.metadata();
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        let line = format!(
            "{seconds} {} {} {}\n",
            metadata.level(),
            metadata.target(),
            words.said
        );
        let _sent = self.lines.send(line);
    }
}

/// Installs the daemon's logging: a rotating file at `path`, at the level
/// [`LEVEL_VARIABLE`] names or `info`.
///
/// # Errors
///
/// [`LoggingError::Io`] when the log file cannot be opened, and
/// [`LoggingError::AlreadyInstalled`] when a subscriber is already in place.
///
/// # Panics
///
/// It spawns the task that owns the file, so it must be called from within a
/// Tokio runtime.
pub async fn initialize(path: &Path) -> Result<(), LoggingError> {
    let mut sink = Sink::open(path, MAXIMUM_LOG_BYTES).await?;
    let (lines, mut waiting) = mpsc::unbounded_channel::<String>();
    let _writing = tokio::spawn(async move {
        while let Some(line) = waiting.recv().await {
            // A log that cannot be written is not worth ending a daemon for,
            // and there is nowhere left to say so.
            let _written = sink.write(&line).await;
        }
    });
    let filter = EnvFilter::try_from_env(LEVEL_VARIABLE)
        .unwrap_or_else(|_unset| EnvFilter::new(DEFAULT_LEVEL));
    tracing_subscriber::registry()
        .with(filter)
        .with(Lines { lines })
        .try_init()
        .map_err(|_installed| LoggingError::AlreadyInstalled)
}
