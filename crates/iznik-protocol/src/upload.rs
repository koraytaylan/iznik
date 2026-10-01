//! A file pasted into a pane, carried in pieces and written by the daemon
//! into that pane's directory.
//!
//! The client reads the file. The server is the only end that can see the
//! directory the pane is in, so the bytes travel as [`FileUpload`] and the
//! server answers with [`UploadAccepted`] once the last piece is in place.
//! A name is a relative path under that directory. A component of `.` or
//! `..`, a leading slash, or a backslash is refused. A name that ends in `/`
//! creates the directory and carries no file bytes. The server keeps a
//! partial file when the link drops, and a later piece continues at the
//! length that file still has.

use crate::frame::MAXIMUM_PAYLOAD_LENGTH;
use crate::identity::PaneId;
use crate::message::MessageError;
use crate::wire::{Reader, Sink, put_bytes, put_optional};

/// The bytes a directory entry may hold, which is what POSIX `NAME_MAX` allows.
const DIRECTORY_ENTRY_BYTES: usize = 255;

/// The prefix of the temporary name a piece is written under, beside the file
/// it will replace. Its length is what [`MAXIMUM_FILE_NAME_BYTES`] leaves room for.
const PARTIAL_PREFIX: &str = ".iznik-partial-";

/// The longest file name an upload accepts, in bytes.
///
/// A directory entry holds what POSIX `NAME_MAX` allows. The temporary name is
/// the partial prefix plus the file's own name, and both have to fit in one
/// entry, so the name itself is shorter by that prefix.
pub const MAXIMUM_FILE_NAME_BYTES: usize = DIRECTORY_ENTRY_BYTES - PARTIAL_PREFIX.len();

/// The most file bytes one [`FileUpload`] carries.
///
/// One less than a frame can hold once the message has named the pane, the
/// file and the offset, measured as if the name were as long as
/// [`MAXIMUM_FILE_NAME_BYTES`]. A shorter name still uses this, so every
/// piece fits whatever the name is.
pub const MAXIMUM_UPLOAD_BYTES: u32 = MAXIMUM_PAYLOAD_LENGTH.saturating_sub(UPLOAD_OVERHEAD);

/// How many file bytes one piece may carry for a name of `name_length` bytes.
///
/// [`MAXIMUM_UPLOAD_BYTES`] assumes the name is [`MAXIMUM_FILE_NAME_BYTES`]
/// long. A relative path can be longer, and then the piece shrinks by the
/// same amount so the frame still fits. `None` when the name alone is too
/// long for a frame, even with no file bytes.
#[must_use]
pub fn piece_limit(name_length: usize) -> Option<u32> {
    let name_bytes = u32::try_from(name_length).ok()?;
    let extra = name_bytes.saturating_sub(NAME_WIRE_BYTES);
    if extra > MAXIMUM_UPLOAD_BYTES {
        return None;
    }
    Some(MAXIMUM_UPLOAD_BYTES.saturating_sub(extra))
}

/// The pane id on the wire, eight bytes.
const PANE_BYTES: u32 = 8;

/// A length prefix, four bytes.
const LENGTH_BYTES: u32 = 4;

/// The file offset, eight bytes.
const OFFSET_BYTES: u32 = 8;

/// The file name's wire size at its longest.
///
/// The same number as [`MAXIMUM_FILE_NAME_BYTES`], at the width the length
/// prefix's arithmetic uses. The two are written out because a `usize` does
/// not become a `u32` inside a constant.
const NAME_WIRE_BYTES: u32 = 240;

/// What an `Upload` spends besides the file bytes it carries.
const UPLOAD_OVERHEAD: u32 = PANE_BYTES
    .saturating_add(LENGTH_BYTES)
    .saturating_add(NAME_WIRE_BYTES)
    .saturating_add(OFFSET_BYTES)
    .saturating_add(LENGTH_BYTES)
    .saturating_add(1)
    .saturating_add(1);

/// One piece of a file being written into a pane's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileUpload {
    /// The pane whose directory receives the file.
    pub pane: PaneId,
    /// The file's name relative to the pane's directory, or a directory when
    /// it ends in `/`. Never absolute, and never a component of `.` or `..`.
    pub name: String,
    /// Where in the file these bytes start. The first piece is zero.
    pub offset: u64,
    /// Whether this piece is the last, and the file should replace any
    /// file already there under [`FileUpload::name`].
    pub finished: bool,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// The server accepted a piece, and names the file when the upload is finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadAccepted {
    /// The pane the file was written for.
    pub pane: PaneId,
    /// How many bytes of the file are in place, including this piece.
    pub written: u64,
    /// The path the file was given, once [`FileUpload::finished`] was set.
    pub path: Option<String>,
}

impl FileUpload {
    /// Appends the piece: the tag is written by the message codec.
    pub(crate) fn write(&self, sink: &mut dyn Sink) {
        sink.put(&self.pane.0.to_le_bytes());
        put_bytes(sink, self.name.as_bytes());
        sink.put(&self.offset.to_le_bytes());
        sink.put(&[u8::from(self.finished)]);
        put_bytes(sink, &self.bytes);
    }

    /// The piece that follows a tag already read.
    ///
    /// # Errors
    ///
    /// [`MessageError`] as the fields' readers give it.
    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<FileUpload, MessageError> {
        Ok(FileUpload {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            name: reader.string()?,
            offset: u64::from_le_bytes(reader.array()?),
            finished: reader.flag()?,
            bytes: reader.bytes()?,
        })
    }
}

impl UploadAccepted {
    /// Appends the acceptance: the tag is written by the message codec.
    pub(crate) fn write(&self, sink: &mut dyn Sink) {
        sink.put(&self.pane.0.to_le_bytes());
        sink.put(&self.written.to_le_bytes());
        put_optional(sink, self.path.as_deref());
    }

    /// The acceptance that follows a tag already read.
    ///
    /// # Errors
    ///
    /// [`MessageError`] as the fields' readers give it.
    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<UploadAccepted, MessageError> {
        let pane = PaneId(u64::from_le_bytes(reader.array()?));
        let written = u64::from_le_bytes(reader.array()?);
        let path = reader.flag()?.then(|| reader.string()).transpose()?;
        Ok(UploadAccepted {
            pane,
            written,
            path,
        })
    }
}
