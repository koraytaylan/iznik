//! A file pasted into a pane, written into that pane's directory.
//!
//! The directory is the one the shell last reported, or — when it has
//! reported none — the directory the pane's process is in on a host that
//! publishes it, or the directory the daemon itself is in. The name is a
//! relative path under that directory: each component is created when it is
//! missing, a link is never followed, and a name ending in `/` creates only
//! the directory. Pieces land in a temporary file beside the destination and
//! replace it only when the last piece says the file is finished. The
//! temporary file is kept when the connection drops, so the next connection
//! can continue from the length it still has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iznik_protocol::identity::PaneId;
use iznik_protocol::message::{ErrorCode, ToClient};
use iznik_protocol::model::HostModel;
use iznik_protocol::upload::{
    FileUpload, MAXIMUM_FILE_NAME_BYTES, MAXIMUM_UPLOAD_BYTES, UploadAccepted,
};
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::RwLock;

use crate::multiplexer::channel::MultiplexerError;
use crate::multiplexer::{FrameSink, Multiplexer};
use crate::session::registry::Registry;

/// The temporary name a piece is written under, beside the file it will replace.
const PARTIAL_PREFIX: &str = ".iznik-partial-";

/// Files this connection is still receiving.
///
/// A file is removed when a piece is refused. A connection that drops keeps
/// the temporary file, which is what a later connection continues.
#[derive(Debug, Default)]
pub(crate) struct UploadState {
    /// One open file per pane.
    open: HashMap<PaneId, Open>,
    /// A resume that started past the temporary file. Later pieces of that
    /// attempt are ignored, and the finished one is told the length the file
    /// still has. The next attempt that starts at that length is written.
    behind: HashMap<PaneId, u64>,
}

/// A file still being received.
#[derive(Debug)]
struct Open {
    /// Where the finished file will be.
    destination: PathBuf,
    /// Where the pieces are written until then.
    partial: PathBuf,
    /// The pieces so far. Taken out to close the file before it is renamed.
    file: Option<tokio::fs::File>,
    /// How many bytes have been written.
    written: u64,
    /// Whether dropping this open file deletes what was written. A dropped
    /// connection leaves the temporary file; a refusal removes it.
    remove_on_drop: bool,
}

impl Drop for Open {
    fn drop(&mut self) {
        self.file.take();
        if self.remove_on_drop {
            let _removed = std::fs::remove_file(&self.partial);
        }
    }
}

/// A name relative to the pane's directory.
struct Relative {
    /// The segments, in order. None is `.`, `..`, or empty.
    segments: Vec<String>,
    /// Whether the name creates a directory and carries no file bytes.
    directory: bool,
}

/// What writing one piece came to.
enum Outcome {
    /// The piece is in place and the file is not finished, so nothing is said
    /// until the last piece: the client sends the pieces without waiting.
    Quiet,
    /// The file is in place.
    Accepted(UploadAccepted),
    /// The piece was refused and the connection stays up.
    Refused {
        /// Why.
        code: ErrorCode,
        /// What a person is told.
        message: String,
    },
}

/// Writes one piece and answers it.
///
/// # Errors
///
/// The link's refusals, when the answer cannot be sent. A piece that cannot
/// be written is answered with [`ToClient::Error`] and is not an error here.
pub(crate) async fn accept<Sink: FrameSink>(
    multiplexer: &mut Multiplexer<Sink>,
    registry: &Arc<RwLock<Registry>>,
    state: &mut UploadState,
    upload: FileUpload,
) -> Result<(), MultiplexerError> {
    let message = match write_piece(registry, state, upload).await {
        Outcome::Quiet => return Ok(()),
        Outcome::Accepted(accepted) => ToClient::UploadAccepted(accepted),
        Outcome::Refused { code, message } => ToClient::Error { code, message },
    };
    multiplexer.reply(&message).await
}

/// Writes one piece, abandoning the file when the piece does not fit it.
async fn write_piece(
    registry: &Arc<RwLock<Registry>>,
    state: &mut UploadState,
    upload: FileUpload,
) -> Outcome {
    let relative = match relative_name(&upload) {
        Ok(relative) => relative,
        Err(message) => return refused(message),
    };
    let directory = match place(registry, upload.pane).await {
        Ok(directory) => directory,
        Err(outcome) => {
            abandon(state, upload.pane);
            return outcome;
        }
    };
    if relative.directory {
        return write_directory(&directory, &relative, &upload).await;
    }
    if let Some(outcome) = catch_up(state, &upload) {
        return outcome;
    }
    if let Err(message) = ensure_open(state, &directory, &relative, &upload).await {
        return message;
    }
    if let Err(message) = append(state, &upload).await {
        abandon(state, upload.pane);
        return refused(message);
    }
    if !upload.finished {
        return Outcome::Quiet;
    }
    match finish(state, upload.pane).await {
        Ok((written, path)) => Outcome::Accepted(UploadAccepted {
            pane: upload.pane,
            written,
            path: Some(path),
        }),
        Err(message) => refused(message),
    }
}

/// A refusal the connection stays up through.
fn refused(message: String) -> Outcome {
    Outcome::Refused {
        code: ErrorCode::Upload,
        message,
    }
}

/// The relative path a piece names, or why it cannot be one.
///
/// # Errors
///
/// A string a person can be shown when the name leaves the directory, names
/// a component this pane cannot receive, or carries more bytes than a frame.
fn relative_name(upload: &FileUpload) -> Result<Relative, String> {
    let piece = u32::try_from(upload.bytes.len()).unwrap_or(u32::MAX) <= MAXIMUM_UPLOAD_BYTES;
    let relative = segments(&upload.name);
    match relative {
        Some(relative) if piece => Ok(relative),
        _ => Err(format!(
            "\"{}\" is not a file name this pane can receive",
            upload.name
        )),
    }
}

/// The segments of a relative name, when every one of them can be created.
fn segments(name: &str) -> Option<Relative> {
    let directory = name.ends_with('/');
    let body = name.trim_end_matches('/');
    let rejected = body.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains("//");
    if rejected {
        return None;
    }
    let mut parts = Vec::new();
    for component in body.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > MAXIMUM_FILE_NAME_BYTES
        {
            return None;
        }
        parts.push(component.to_owned());
    }
    Some(Relative {
        segments: parts,
        directory,
    })
}

/// Creates the directory a piece named, and answers once it exists.
async fn write_directory(root: &Path, relative: &Relative, upload: &FileUpload) -> Outcome {
    if !upload.finished || upload.offset != 0 || !upload.bytes.is_empty() {
        return refused(format!(
            "\"{}\" is not a directory this pane can receive",
            upload.name
        ));
    }
    match make_path(root, &relative.segments).await {
        Ok(path) => Outcome::Accepted(UploadAccepted {
            pane: upload.pane,
            written: 0,
            path: Some(path.display().to_string()),
        }),
        Err(message) => refused(message),
    }
}

/// Ignores pieces of an attempt that started past the temporary file.
///
/// The finished piece is the one answer, so the client can send the rest
/// without reading. An attempt that starts at the length the file still has
/// is written, and this returns nothing so the caller opens it.
fn catch_up(state: &mut UploadState, upload: &FileUpload) -> Option<Outcome> {
    if upload.offset == 0 {
        let _cleared = state.behind.remove(&upload.pane);
        return None;
    }
    let at = state.behind.get(&upload.pane).copied()?;
    if upload.offset == at {
        let _cleared = state.behind.remove(&upload.pane);
        return None;
    }
    Some(behind(state, upload, at))
}

/// Remembers that `pane` is at `at`, and answers when this piece is the last.
fn behind(state: &mut UploadState, upload: &FileUpload, at: u64) -> Outcome {
    let _held = state.behind.insert(upload.pane, at);
    if upload.finished {
        refused(format!("\"{}\" is at byte {at}", upload.name))
    } else {
        Outcome::Quiet
    }
}

/// Opens the temporary file this piece continues, or answers why it cannot.
///
/// # Errors
///
/// [`Outcome`] when the file cannot be started or continued. A resume that
/// starts past the temporary file is not an error the caller abandons: the
/// temporary file stays, and the finished piece names its length.
async fn ensure_open(
    state: &mut UploadState,
    directory: &Path,
    relative: &Relative,
    upload: &FileUpload,
) -> Result<(), Outcome> {
    if upload.offset == 0 {
        abandon(state, upload.pane);
        let open = begin(directory, relative).await.map_err(refused)?;
        drop(state.open.insert(upload.pane, open));
        return Ok(());
    }
    if state.open.contains_key(&upload.pane) {
        return Ok(());
    }
    match open_partial(directory, relative, upload.offset).await {
        Ok(open) => {
            drop(state.open.insert(upload.pane, open));
            Ok(())
        }
        Err(OpenError::Position(at)) => Err(behind(state, upload, at)),
        Err(OpenError::Detail(message)) => {
            abandon(state, upload.pane);
            Err(refused(message))
        }
    }
}

/// Why a temporary file could not be continued.
enum OpenError {
    /// The temporary file is this long, which is not the offset that arrived.
    Position(u64),
    /// The file cannot be continued, and what was written should be removed.
    Detail(String),
}

/// Drops a pane's open file and deletes what was written so far.
fn abandon(state: &mut UploadState, pane: PaneId) {
    let _held = state.behind.remove(&pane);
    if let Some(mut open) = state.open.remove(&pane) {
        open.remove_on_drop = true;
    }
}

/// The directory a pane's file is written into.
///
/// # Errors
///
/// [`Outcome::Refused`] when the pane does not exist or no directory can be
/// found for it.
async fn place(registry: &Arc<RwLock<Registry>>, pane: PaneId) -> Result<PathBuf, Outcome> {
    let (reported, process) = {
        let guard = registry.read().await;
        let model = guard.snapshot();
        let Located::Present(reported) = pane_directory(&model, pane) else {
            return Err(Outcome::Refused {
                code: ErrorCode::UnknownPane,
                message: format!("the host holds no pane {}", pane.0),
            });
        };
        let process = guard.pane(pane).map(|pane| pane.process_id());
        (reported, process)
    };
    if let Some(path) = reported.as_deref()
        && let Some(directory) = directory_of(Path::new(path)).await
    {
        return Ok(directory);
    }
    if let Some(process) = process
        && let Some(path) = process_directory(process)
        && let Some(directory) = directory_of(&path).await
    {
        return Ok(directory);
    }
    if let Ok(current) = std::env::current_dir()
        && let Some(directory) = directory_of(&current).await
    {
        return Ok(directory);
    }
    Err(Outcome::Refused {
        code: ErrorCode::Upload,
        message: format!("pane {} has not reported a directory", pane.0),
    })
}

/// Where a pane sits in the model, and the directory it reported.
enum Located {
    /// The model holds no such pane.
    Absent,
    /// The pane is there. The directory is what it last reported.
    Present(Option<String>),
}

/// The pane's reported directory, or [`Located::Absent`] when it is not there.
fn pane_directory(model: &HostModel, pane: PaneId) -> Located {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .find(|held| held.id == pane)
        .map_or(Located::Absent, |held| {
            Located::Present(held.working_directory.clone())
        })
}

/// A path that exists and is a directory, canonical so a later join cannot
/// leave it by a link.
async fn directory_of(path: &Path) -> Option<PathBuf> {
    let canonical = tokio::fs::canonicalize(path).await.ok()?;
    canonical.is_dir().then_some(canonical)
}

/// The directory a process is in, on a host that publishes it through `/proc`.
fn process_directory(process: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{process}/cwd")).ok()
}

/// Opens the temporary file a pane's upload is written to, replacing one a
/// previous attempt left behind.
///
/// # Errors
///
/// A string a person can be shown when the name leaves the directory, names
/// something that is not a regular file, or the temporary file cannot be opened.
async fn begin(directory: &Path, relative: &Relative) -> Result<Open, String> {
    let (destination, partial) = file_paths(directory, relative).await?;
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&partial)
        .await
        .map_err(|error| format!("\"{}\" could not be started: {error}", partial.display()))?;
    Ok(Open {
        destination,
        partial,
        file: Some(file),
        written: 0,
        remove_on_drop: false,
    })
}

/// Opens a temporary file a previous connection left, at `offset`.
///
/// When the file is longer than `offset`, it is cut back to `offset` and the
/// new piece replaces that tail. When it is shorter, or missing, the error
/// names the length a later attempt should start at.
///
/// # Errors
///
/// [`OpenError::Position`] when the piece does not continue the file, and
/// [`OpenError::Detail`] when the file cannot be opened.
async fn open_partial(
    directory: &Path,
    relative: &Relative,
    offset: u64,
) -> Result<Open, OpenError> {
    let (destination, partial) = file_paths(directory, relative)
        .await
        .map_err(OpenError::Detail)?;
    let length = match tokio::fs::symlink_metadata(&partial).await {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        Ok(_) => {
            return Err(OpenError::Detail(format!(
                "\"{}\" is not a file this pane can continue",
                partial.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(OpenError::Position(0));
        }
        Err(error) => {
            return Err(OpenError::Detail(format!(
                "\"{}\" could not be read: {error}",
                partial.display()
            )));
        }
    };
    if offset > length {
        return Err(OpenError::Position(length));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&partial)
        .await
        .map_err(|error| {
            OpenError::Detail(format!(
                "\"{}\" could not be continued: {error}",
                partial.display()
            ))
        })?;
    if offset < length {
        file.set_len(offset).await.map_err(|error| {
            OpenError::Detail(format!(
                "\"{}\" could not be continued: {error}",
                partial.display()
            ))
        })?;
    }
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|error| {
            OpenError::Detail(format!(
                "\"{}\" could not be continued: {error}",
                partial.display()
            ))
        })?;
    Ok(Open {
        destination,
        partial,
        file: Some(file),
        written: offset,
        remove_on_drop: false,
    })
}

/// The destination and the temporary file beside it, with every parent created.
///
/// # Errors
///
/// A string a person can be shown when a parent cannot be created, a component
/// is a link, or the destination is not a regular file.
async fn file_paths(directory: &Path, relative: &Relative) -> Result<(PathBuf, PathBuf), String> {
    let last = relative
        .segments
        .last()
        .ok_or_else(|| "the file has no name this pane can receive".to_owned())?;
    let parent_count = relative.segments.len().saturating_sub(1);
    let prefix_segments = relative
        .segments
        .get(..parent_count)
        .ok_or_else(|| "the file has no name this pane can receive".to_owned())?;
    let parent = make_path(directory, prefix_segments).await?;
    let destination = parent.join(last);
    if let Ok(metadata) = tokio::fs::symlink_metadata(&destination).await
        && (metadata.file_type().is_symlink() || metadata.is_dir())
    {
        return Err(format!(
            "\"{}\" is not a file this pane can replace",
            destination.display()
        ));
    }
    let partial = parent.join(format!("{PARTIAL_PREFIX}{last}"));
    Ok((destination, partial))
}

/// Creates every component under `root`, refusing a link and a path that
/// leaves `root` once canonical.
///
/// # Errors
///
/// A string a person can be shown when a component cannot be created or read.
async fn make_path(root: &Path, segments: &[String]) -> Result<PathBuf, String> {
    let mut current = root.to_path_buf();
    for component in segments {
        current = step(&current, root, component).await?;
    }
    Ok(current)
}

/// One component under `current`, created when it is missing.
///
/// # Errors
///
/// A string a person can be shown when the component is a link, is not a
/// directory, or cannot be created inside `root`.
async fn step(current: &Path, root: &Path, component: &str) -> Result<PathBuf, String> {
    let next = current.join(component);
    match tokio::fs::symlink_metadata(&next).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("\"{component}\" is a link"))
        }
        Ok(metadata) if metadata.is_dir() => canonical_under(&next, root, component).await,
        Ok(_) => Err(format!("\"{component}\" is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(&next)
                .await
                .map_err(|create| format!("\"{component}\" could not be created: {create}"))?;
            canonical_under(&next, root, component).await
        }
        Err(error) => Err(format!("\"{component}\" could not be read: {error}")),
    }
}

/// `path` canonical, when it is still inside `root`.
///
/// # Errors
///
/// A string a person can be shown when the path cannot be read or leaves `root`.
async fn canonical_under(path: &Path, root: &Path, component: &str) -> Result<PathBuf, String> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| format!("\"{component}\" could not be read: {error}"))?;
    if canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(format!("\"{component}\" leaves the pane's directory"))
    }
}

/// Appends a piece that continues the open file at the offset it claims.
///
/// # Errors
///
/// A string a person can be shown when the piece does not continue the file
/// or the write fails.
async fn append(state: &mut UploadState, upload: &FileUpload) -> Result<(), String> {
    let Some(open) = state.open.get_mut(&upload.pane) else {
        return Err("the file was not started at the beginning".to_owned());
    };
    if upload.offset != open.written {
        return Err(format!(
            "\"{}\" continued at byte {}, and the file is at byte {}",
            upload.name, upload.offset, open.written
        ));
    }
    let added = u64::try_from(upload.bytes.len()).unwrap_or(u64::MAX);
    let Some(written) = open.written.checked_add(added) else {
        return Err(format!("\"{}\" is too long to write", upload.name));
    };
    let Some(file) = open.file.as_mut() else {
        return Err(format!("\"{}\" was already closed", upload.name));
    };
    file.write_all(&upload.bytes)
        .await
        .map_err(|error| format!("\"{}\" could not be written: {error}", upload.name))?;
    file.sync_all()
        .await
        .map_err(|error| format!("\"{}\" could not be saved: {error}", upload.name))?;
    open.written = written;
    Ok(())
}

/// Closes the file and puts it under its name.
///
/// # Errors
///
/// A string a person can be shown when the file cannot be saved or renamed.
async fn finish(state: &mut UploadState, pane: PaneId) -> Result<(u64, String), String> {
    let Some(mut open) = state.open.remove(&pane) else {
        return Err("the file was not started".to_owned());
    };
    let written = open.written;
    let destination = open.destination.clone();
    let partial = open.partial.clone();
    if let Some(file) = open.file.take() {
        file.sync_all()
            .await
            .map_err(|error| format!("the file could not be saved: {error}"))?;
    }
    // Closed before the rename: a host that will not rename an open file
    // still replaces the destination.
    replace(&partial, &destination)
        .await
        .map_err(|error| format!("the file could not be put in place: {error}"))?;
    Ok((written, destination.display().to_string()))
}

/// Moves `partial` onto `destination`, replacing a regular file that is
/// already there. A directory or a link was refused before the pieces began.
///
/// # Errors
///
/// The operating system's refusal, when the file cannot be renamed into place.
async fn replace(partial: &Path, destination: &Path) -> std::io::Result<()> {
    match tokio::fs::rename(partial, destination).await {
        Ok(()) => Ok(()),
        Err(_error) => {
            tokio::fs::remove_file(destination).await?;
            tokio::fs::rename(partial, destination).await
        }
    }
}
