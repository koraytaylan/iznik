//! Sending a pasted file to the host that holds the pane.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::ToServer;
use iznik_protocol::upload::{FileUpload, piece_limit};
use tokio::io::{AsyncReadExt, AsyncSeekExt as _};

use crate::host::identity::HostId;
use crate::host::manager::task::write;
use crate::host::manager::{HostManager, ManagerError, ManagerEvent, Order, Shared};
use crate::transport::channel::{ChannelError, RemoteChannel};

impl HostManager {
    /// Sends the file at `path` into a pane's directory, under `name`.
    ///
    /// The host's task reads the file and writes it in pieces. What comes
    /// back is a [`ManagerEvent::UploadFinished`] when the file is in place, or
    /// [`ManagerEvent::UploadFailed`] when it is not.
    ///
    /// # Errors
    ///
    /// [`ManagerError::UnknownHost`], [`ManagerError::Gone`], and
    /// [`ManagerError::Unsupported`] when the connected server did not
    /// advertise [`Capabilities::UPLOAD`]. The file itself is not opened here.
    pub fn upload_file(
        &self,
        alias: &str,
        pane: PaneId,
        name: String,
        path: PathBuf,
        offset: u64,
    ) -> Result<(), ManagerError> {
        let host = HostId(alias.to_owned());
        let supported = self
            .shared
            .with(&host, |view| {
                view.capabilities.contains(Capabilities::UPLOAD)
            })
            .ok_or_else(|| ManagerError::UnknownHost { host: host.clone() })?;
        if !supported {
            return Err(ManagerError::Unsupported {
                host,
                command: "upload",
            });
        }
        self.order(
            alias,
            Order::Upload {
                pane,
                name,
                path,
                offset,
            },
        )
    }
}

/// Tells the application how much of a file has been sent.
fn publish_progress(
    host: &HostId,
    shared: &Shared,
    pane: PaneId,
    name: &str,
    sent: u64,
    total: u64,
) {
    shared.publish(ManagerEvent::UploadProgress {
        host: host.clone(),
        pane,
        name: name.to_owned(),
        sent,
        total,
    });
}

/// Tells the application a file will not be written.
pub(super) fn failed(host: &HostId, shared: &Shared, pane: PaneId, detail: String) {
    shared.publish(ManagerEvent::UploadFailed {
        host: host.clone(),
        pane,
        detail,
    });
}

/// Reads `path` and writes it to the pane as `Upload` pieces, in order.
///
/// A file that cannot be read is reported and is not a link failure: the
/// connection never saw a byte of it. A piece the link will not take is.
///
/// # Errors
///
/// [`ChannelError`] when a piece cannot be written on the link.
pub(super) async fn carry(
    host: &HostId,
    shared: &Arc<Shared>,
    channel: &mut RemoteChannel,
    pane: PaneId,
    name: String,
    path: PathBuf,
    offset: u64,
) -> Result<(), ChannelError> {
    if let Err(detail) = send_file(host, shared, channel, pane, &name, &path, offset).await {
        match detail {
            SendError::Local(detail) => failed(host, shared, pane, detail),
            SendError::Link(error) => {
                failed(
                    host,
                    shared,
                    pane,
                    "the link failed before the file was in place".to_owned(),
                );
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Why a file was not sent.
enum SendError {
    /// The file on this machine could not be read, and nothing was sent.
    Local(String),
    /// A piece was not accepted by the link.
    Link(ChannelError),
}

/// Sends every piece of one file.
///
/// # Errors
///
/// [`SendError::Local`] when the file cannot be read, and [`SendError::Link`]
/// when a piece cannot be written on the link.
async fn send_file(
    host: &HostId,
    shared: &Arc<Shared>,
    channel: &mut RemoteChannel,
    pane: PaneId,
    name: &str,
    path: &Path,
    mut offset: u64,
) -> Result<(), SendError> {
    if name.is_empty() {
        return Err(SendError::Local(format!(
            "\"{name}\" is not a file name the host can receive"
        )));
    }
    let Some(room) = piece_limit(name.len()) else {
        return Err(SendError::Local(format!(
            "\"{name}\" is not a file name the host can receive"
        )));
    };
    if name.ends_with('/') {
        publish_progress(host, shared, pane, name, 0, 0);
        return write_piece(channel, pane, name, 0, true, Vec::new())
            .await
            .map_err(SendError::Link);
    }
    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        SendError::Local(format!("{} could not be read: {error}", path.display()))
    })?;
    let length = file
        .metadata()
        .await
        .map_err(|error| {
            SendError::Local(format!("{} could not be read: {error}", path.display()))
        })?
        .len();
    if offset > length {
        return Err(SendError::Local(format!(
            "{} is shorter than the {} bytes already sent",
            path.display(),
            offset
        )));
    }
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|error| {
            SendError::Local(format!("{} could not be read: {error}", path.display()))
        })?;
    if room == 0 && length > offset {
        return Err(SendError::Local(format!(
            "\"{name}\" is not a file name the host can receive"
        )));
    }
    publish_progress(host, shared, pane, name, offset, length);
    let most = usize::try_from(room).unwrap_or(1);
    loop {
        let remaining = length.saturating_sub(offset);
        if remaining == 0 {
            return write_piece(channel, pane, name, offset, true, Vec::new())
                .await
                .map_err(SendError::Link);
        }
        let want = usize::try_from(remaining).unwrap_or(most).min(most);
        let mut buffer = vec![0; want];
        let read = file.read(&mut buffer).await.map_err(|error| {
            SendError::Local(format!("{} could not be read: {error}", path.display()))
        })?;
        if read == 0 {
            return Err(SendError::Local(format!(
                "{} ended before its {} bytes were read",
                path.display(),
                length
            )));
        }
        buffer.truncate(read);
        let added = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let Some(next) = offset.checked_add(added) else {
            return Err(SendError::Local(format!(
                "{} is too long to send",
                path.display()
            )));
        };
        let finished = next == length;
        write_piece(channel, pane, name, offset, finished, buffer)
            .await
            .map_err(SendError::Link)?;
        publish_progress(host, shared, pane, name, next, length);
        offset = next;
        if finished {
            return Ok(());
        }
    }
}

/// Writes one piece.
///
/// # Errors
///
/// [`ChannelError`] when the link will not take the piece.
async fn write_piece(
    channel: &mut RemoteChannel,
    pane: PaneId,
    name: &str,
    offset: u64,
    finished: bool,
    bytes: Vec<u8>,
) -> Result<(), ChannelError> {
    write(
        channel,
        &ToServer::Upload(FileUpload {
            pane,
            name: name.to_owned(),
            offset,
            finished,
            bytes,
        }),
    )
    .await
}
