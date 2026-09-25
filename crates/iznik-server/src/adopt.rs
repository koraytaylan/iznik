//! The record one daemon writes before it is replaced and the next one reads.
//!
//! It is a file in the runtime directory, never a client message. The bytes
//! are the protocol's own length-prefixed primitives, and a record this build
//! does not know — another version, a field cut short, bytes left over — is
//! refused by name rather than guessed.

use std::path::Path;

use iznik_protocol::identity::{DaemonInstance, PaneId, Sequence};
use iznik_protocol::message::MessageError;
use iznik_protocol::model::{HostModel, decode_host_model, encode_host_model};
use iznik_protocol::wire::{Reader, put_bytes, put_count};

/// The only record version this build writes or reads.
pub const ADOPTED_STATE_VERSION: u16 = 1;

/// How wide the version field is.
const VERSION_WIDTH: usize = 2;

/// How wide a length prefix is. A body that claims more than this and then
/// runs out is an oversized field rather than a header cut in half.
const LENGTH_WIDTH: usize = 4;

/// The mode of the record: the user who owns the runtime directory, and
/// nobody else. The directory itself is `0700`.
#[cfg(unix)]
const OWNER_READ_WRITE: u32 = 0o600;

/// Everything the next process needs to be the same daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedState {
    /// [`ADOPTED_STATE_VERSION`] for a record this build wrote.
    pub version: u16,
    /// The run the clients already know. A replacement that kept the sessions
    /// is this same run, so a reconnect resumes instead of starting over.
    pub instance: DaemonInstance,
    /// The sessions, tabs and panes, as the clients last saw them.
    pub model: HostModel,
    /// One entry for every pane the model names.
    pub panes: Vec<AdoptedPane>,
}

/// One pane carried across the replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedPane {
    /// The pane's identity, which does not change.
    pub pane: PaneId,
    /// The pseudoterminal master, as a descriptor number in this process.
    pub descriptor: i32,
    /// The child's process id.
    pub process_id: u32,
    /// The sequence just past the newest byte the pane has produced.
    pub sequence: Sequence,
    /// The bytes the history ring still holds, ending at [`Self::sequence`].
    pub ring: Vec<u8>,
    /// The terminal attributes, carried so they can be seen. The kernel object
    /// already has them; the next process does not apply them again.
    pub termios: TermiosState,
}

/// The terminal attributes of one master, as the bytes `tcgetattr` reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermiosState {
    /// The attributes, empty when the descriptor had none.
    pub bytes: Vec<u8>,
}

/// Why a record could not be written, read, or acted on.
#[derive(Debug)]
pub enum AdoptError {
    /// The record names a version this build does not read.
    Version {
        /// The version the record named.
        found: u16,
    },
    /// A field ended before its bytes did.
    Truncated,
    /// A length prefix claimed more bytes than the record held.
    Oversized {
        /// How many the field claimed.
        needed: usize,
        /// How many were left.
        available: usize,
    },
    /// Bytes follow the last field.
    Trailing {
        /// How many.
        count: usize,
    },
    /// The record is not one this build can read, for a reason of its own.
    Malformed {
        /// What was wrong.
        detail: String,
    },
    /// A master the record names is not an open descriptor.
    Master {
        /// The descriptor number.
        descriptor: i32,
    },
    /// The staged binary could not be executed. The calling process is still
    /// the daemon.
    Execute {
        /// The binary.
        path: std::path::PathBuf,
        /// What the operating system said.
        detail: String,
    },
    /// A pane's child is already gone, so there is nothing to keep.
    Ended {
        /// The pane.
        pane: PaneId,
    },
    /// The record could not be written or read.
    Io {
        /// What was wrong.
        detail: String,
    },
}

impl core::fmt::Display for AdoptError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AdoptError::Version { found } => write!(
                formatter,
                "adopted state version {found} is not {ADOPTED_STATE_VERSION}"
            ),
            AdoptError::Truncated => formatter.write_str("adopted state is truncated"),
            AdoptError::Oversized { needed, available } => write!(
                formatter,
                "adopted state field is oversized: it claims {needed} bytes and {available} remain"
            ),
            AdoptError::Trailing { count } => {
                write!(formatter, "adopted state has {count} trailing bytes")
            }
            AdoptError::Malformed { detail } => {
                write!(formatter, "adopted state is malformed: {detail}")
            }
            AdoptError::Master { descriptor } => {
                write!(formatter, "master descriptor {descriptor} is not open")
            }
            AdoptError::Execute { path, detail } => {
                write!(formatter, "could not execute {}: {detail}", path.display())
            }
            AdoptError::Ended { pane } => write!(formatter, "pane {} has already ended", pane.0),
            AdoptError::Io { detail } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for AdoptError {}

impl From<MessageError> for AdoptError {
    fn from(error: MessageError) -> AdoptError {
        match error {
            MessageError::TrailingBytes { count, .. } => AdoptError::Trailing { count },
            MessageError::Truncated {
                needed, available, ..
            } if needed > available && needed > LENGTH_WIDTH => {
                AdoptError::Oversized { needed, available }
            }
            MessageError::Truncated { .. } => AdoptError::Truncated,
            other => AdoptError::Malformed {
                detail: other.to_string(),
            },
        }
    }
}

/// The bytes of `state`.
///
/// # Errors
///
/// [`AdoptError::Malformed`] when the host model itself will not encode.
pub fn encode_adopted(state: &AdoptedState) -> Result<Vec<u8>, AdoptError> {
    let model = encode_host_model(&state.model).map_err(|error| AdoptError::Malformed {
        detail: error.to_string(),
    })?;
    let mut out = Vec::new();
    out.extend_from_slice(&state.version.to_le_bytes());
    out.extend_from_slice(&state.instance.0.to_le_bytes());
    put_bytes(&mut out, &model);
    put_count(&mut out, state.panes.len());
    for pane in &state.panes {
        write_pane(&mut out, pane);
    }
    Ok(out)
}

/// Appends one pane. The lengths are the protocol's own prefixes.
fn write_pane(out: &mut Vec<u8>, pane: &AdoptedPane) {
    out.extend_from_slice(&pane.pane.0.to_le_bytes());
    out.extend_from_slice(&pane.descriptor.to_le_bytes());
    out.extend_from_slice(&pane.process_id.to_le_bytes());
    out.extend_from_slice(&pane.sequence.0.to_le_bytes());
    put_bytes(out, &pane.ring);
    put_bytes(out, &pane.termios.bytes);
}

/// The state `bytes` carries.
///
/// # Errors
///
/// [`AdoptError::Version`] for another version, [`AdoptError::Truncated`] and
/// [`AdoptError::Oversized`] for a field that is not all there,
/// [`AdoptError::Trailing`] when bytes follow the last pane, and
/// [`AdoptError::Malformed`] when the model itself is not one this build reads.
pub fn decode_adopted(bytes: &[u8]) -> Result<AdoptedState, AdoptError> {
    if bytes.len() < VERSION_WIDTH {
        return Err(AdoptError::Truncated);
    }
    let mut reader = Reader::payload(bytes);
    let version = u16::from_le_bytes(reader.array()?);
    if version != ADOPTED_STATE_VERSION {
        return Err(AdoptError::Version { found: version });
    }
    let instance = DaemonInstance(u128::from_le_bytes(reader.array()?));
    let model = decode_host_model(&reader.bytes()?)?;
    let count = reader.count()?;
    let mut panes = Vec::new();
    for _index in 0..count {
        panes.push(read_pane(&mut reader)?);
    }
    reader.finish()?;
    Ok(AdoptedState {
        version,
        instance,
        model,
        panes,
    })
}

/// The pane at `reader`.
///
/// # Errors
///
/// As [`decode_adopted`].
fn read_pane(reader: &mut Reader<'_>) -> Result<AdoptedPane, AdoptError> {
    let pane = PaneId(u64::from_le_bytes(reader.array()?));
    let descriptor = i32::from_le_bytes(reader.array()?);
    let process_id = u32::from_le_bytes(reader.array()?);
    let sequence = Sequence(u64::from_le_bytes(reader.array()?));
    let ring = reader.bytes()?;
    let termios = TermiosState {
        bytes: reader.bytes()?,
    };
    Ok(AdoptedPane {
        pane,
        descriptor,
        process_id,
        sequence,
        ring,
        termios,
    })
}

/// Writes `state` at `path`, readable and writable by this user only.
///
/// # Errors
///
/// [`AdoptError::Io`] when the file cannot be written, and the encoding
/// refusals of [`encode_adopted`].
pub fn write_state(path: &Path, state: &AdoptedState) -> Result<(), AdoptError> {
    let bytes = encode_adopted(state)?;
    std::fs::write(path, &bytes).map_err(|source| AdoptError::Io {
        detail: format!("writing {}: {source}", path.display()),
    })?;
    restrict(path)?;
    Ok(())
}

/// Reads the state at `path`.
///
/// # Errors
///
/// [`AdoptError::Io`] when the file cannot be read, and the refusals of
/// [`decode_adopted`].
pub fn read_state(path: &Path) -> Result<AdoptedState, AdoptError> {
    let bytes = std::fs::read(path).map_err(|source| AdoptError::Io {
        detail: format!("reading {}: {source}", path.display()),
    })?;
    decode_adopted(&bytes)
}

/// Restricts `path` to its owner on Unix. Elsewhere the directory's own
/// access control is what keeps the record private.
///
/// # Errors
///
/// [`AdoptError::Io`] when the mode cannot be set.
fn restrict(path: &Path) -> Result<(), AdoptError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(OWNER_READ_WRITE)).map_err(
            |source| AdoptError::Io {
                detail: format!("restricting {}: {source}", path.display()),
            },
        )?;
    }
    #[cfg(not(unix))]
    {
        let _path = path;
    }
    Ok(())
}
