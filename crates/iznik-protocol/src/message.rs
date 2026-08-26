//! The `iznik/1` control messages on channel 0 in both directions, the error
//! codes and the mark kinds, and the rule that pane output on every other
//! channel is never parsed. The golden `tests/fixtures/message.jsonl` pins
//! every byte; this code is held to it.
//!
//! On the wire a message is a one-byte discriminant and then its fields in
//! the order the architecture tabulates them: integers little-endian at
//! their width, a string or an opaque payload a four-byte length and then
//! the bytes, an optional exit status a presence byte and then the value, a
//! boolean one byte. A discriminant, error code, mark kind, presence or
//! boolean byte no variant claims is refused as unknown; bytes missing or
//! left over are refused as such. The session-model payloads are opaque here
//! so plan 0003 can define them without moving a byte this golden pins.

use core::fmt::{self, Display, Formatter};

use crate::capabilities::Capabilities;
use crate::frame::MAXIMUM_PAYLOAD_LENGTH;
use crate::identity::{CommandId, Generation, PaneId, Sequence};

/// The channel control messages travel on; every other channel carries pane
/// output.
pub const CHANNEL_CONTROL: u8 = 0;

/// The protocol version this codec speaks. A `Hello` naming another version
/// decodes to that value; the handshake owns the refusal.
pub const PROTOCOL_VERSION: u16 = 1;

/// The byte that says an optional value is absent, and the boolean `false`.
const ABSENT: u8 = 0;

/// The byte that says an optional value follows, and the boolean `true`.
const PRESENT: u8 = 1;

/// The discriminants of [`ToServer`], in table order.
mod server_tag {
    /// `Hello`.
    pub(super) const HELLO: u8 = 0;
    /// `SnapshotRequest`.
    pub(super) const SNAPSHOT_REQUEST: u8 = 1;
    /// `Command`.
    pub(super) const COMMAND: u8 = 2;
    /// `Subscribe`.
    pub(super) const SUBSCRIBE: u8 = 3;
    /// `Unsubscribe`.
    pub(super) const UNSUBSCRIBE: u8 = 4;
    /// `Resume`.
    pub(super) const RESUME: u8 = 5;
    /// `ScreenRequest`.
    pub(super) const SCREEN_REQUEST: u8 = 6;
    /// `Credit`.
    pub(super) const CREDIT: u8 = 7;
    /// `ChannelReleased`.
    pub(super) const CHANNEL_RELEASED: u8 = 8;
    /// `Input`.
    pub(super) const INPUT: u8 = 9;
    /// `Resize`.
    pub(super) const RESIZE: u8 = 10;
    /// `Focus`.
    pub(super) const FOCUS: u8 = 11;
    /// `Ping`.
    pub(super) const PING: u8 = 12;
}

/// The discriminants of [`ToClient`], in table order.
mod client_tag {
    /// `Hello`.
    pub(super) const HELLO: u8 = 0;
    /// `Snapshot`.
    pub(super) const SNAPSHOT: u8 = 1;
    /// `Delta`.
    pub(super) const DELTA: u8 = 2;
    /// `CommandResult`.
    pub(super) const COMMAND_RESULT: u8 = 3;
    /// `PaneChannel`.
    pub(super) const PANE_CHANNEL: u8 = 4;
    /// `PaneDetached`.
    pub(super) const PANE_DETACHED: u8 = 5;
    /// `Screen`.
    pub(super) const SCREEN: u8 = 6;
    /// `Mark`.
    pub(super) const MARK: u8 = 7;
    /// `Pong`.
    pub(super) const PONG: u8 = 8;
    /// `Error`.
    pub(super) const ERROR: u8 = 9;
}

/// The wire values of [`ErrorCode`], in declaration order.
mod error_tag {
    /// `ProtocolVersion`.
    pub(super) const PROTOCOL_VERSION: u8 = 0;
    /// `InputBacklog`.
    pub(super) const INPUT_BACKLOG: u8 = 1;
    /// `UnknownPane`.
    pub(super) const UNKNOWN_PANE: u8 = 2;
    /// `ChannelsExhausted`.
    pub(super) const CHANNELS_EXHAUSTED: u8 = 3;
    /// `NotSubscribed`.
    pub(super) const NOT_SUBSCRIBED: u8 = 4;
}

/// The wire values of [`MarkKind`], in declaration order.
mod mark_tag {
    /// `PromptStart`.
    pub(super) const PROMPT_START: u8 = 0;
    /// `CommandStart`.
    pub(super) const COMMAND_START: u8 = 1;
    /// `CommandExecuted`.
    pub(super) const COMMAND_EXECUTED: u8 = 2;
    /// `CommandFinished`.
    pub(super) const COMMAND_FINISHED: u8 = 3;
    /// `WorkingDirectory`.
    pub(super) const WORKING_DIRECTORY: u8 = 4;
    /// `Title`.
    pub(super) const TITLE: u8 = 5;
    /// `AlternateScreen`.
    pub(super) const ALTERNATE_SCREEN: u8 = 6;
}

/// A message from a client to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToServer {
    /// The first message on every connection.
    Hello {
        /// The protocol version the client speaks.
        protocol_version: u16,
        /// The client's own version, for logs.
        client_version: String,
        /// What the client can do.
        capabilities: Capabilities,
    },
    /// Ask for the complete host model.
    SnapshotRequest,
    /// A session command, answered by [`ToClient::CommandResult`].
    Command {
        /// The client's number for the command.
        command_id: CommandId,
        /// The command, opaque here; plan 0003 defines it.
        payload: Vec<u8>,
    },
    /// Begin delivery of a pane's output.
    Subscribe {
        /// The pane.
        pane: PaneId,
    },
    /// End delivery of a pane's output.
    Unsubscribe {
        /// The pane.
        pane: PaneId,
    },
    /// Subscribe and continue from a byte position the client already holds.
    Resume {
        /// The pane.
        pane: PaneId,
        /// The first byte the client does not hold.
        from_sequence: Sequence,
    },
    /// Ask for the pane's current screen as VT bytes.
    ScreenRequest {
        /// The pane.
        pane: PaneId,
    },
    /// Return flow-control credit for a pane channel.
    Credit {
        /// The channel.
        channel: u8,
        /// The bytes the client has consumed.
        bytes: u32,
    },
    /// Acknowledge a [`ToClient::PaneDetached`], allowing the channel number
    /// to be reused.
    ChannelReleased {
        /// The channel.
        channel: u8,
    },
    /// Keystrokes, paste, and the client emulator's own query responses.
    Input {
        /// The pane.
        pane: PaneId,
        /// The bytes, exactly as typed or answered.
        bytes: Vec<u8>,
    },
    /// Set a pane's size.
    Resize {
        /// The pane.
        pane: PaneId,
        /// The width in cells.
        columns: u16,
        /// The height in cells.
        rows: u16,
    },
    /// Which pane this client is looking at, for scheduling priority.
    Focus {
        /// The pane.
        pane: PaneId,
    },
    /// Liveness.
    Ping,
}

/// A message from the server to a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToClient {
    /// The handshake reply.
    Hello {
        /// The protocol version the server speaks.
        protocol_version: u16,
        /// The server's own version, for logs.
        server_version: String,
        /// What the server can do.
        capabilities: Capabilities,
    },
    /// The complete host model.
    Snapshot {
        /// The model's generation.
        generation: Generation,
        /// The model, opaque here; plan 0003 defines it.
        payload: Vec<u8>,
    },
    /// One change to the model, numbered.
    Delta {
        /// The generation the change produces.
        generation: Generation,
        /// The change, opaque here; plan 0003 defines it.
        payload: Vec<u8>,
    },
    /// The answer to a [`ToServer::Command`].
    CommandResult {
        /// The client's number for the command.
        command_id: CommandId,
        /// The outcome, opaque here; plan 0003 defines it.
        payload: Vec<u8>,
    },
    /// The channel a subscribed pane's output flows on, and the byte
    /// position the stream starts at.
    PaneChannel {
        /// The pane.
        pane: PaneId,
        /// The channel.
        channel: u8,
        /// The sequence of the first byte the channel will carry.
        sequence: Sequence,
    },
    /// Output for the pane has stopped on that channel.
    PaneDetached {
        /// The pane.
        pane: PaneId,
        /// The channel.
        channel: u8,
    },
    /// The pane's screen and scrollback as VT bytes, exact at `sequence`.
    Screen {
        /// The pane.
        pane: PaneId,
        /// The sequence the screen is exact at.
        sequence: Sequence,
        /// The width in cells.
        columns: u16,
        /// The height in cells.
        rows: u16,
        /// The VT bytes that reproduce the screen.
        bytes: Vec<u8>,
    },
    /// A shell-integration event.
    Mark {
        /// The pane.
        pane: PaneId,
        /// The sequence the event sits at.
        sequence: Sequence,
        /// What happened.
        kind: MarkKind,
    },
    /// Liveness.
    Pong,
    /// A refusal.
    Error {
        /// Why.
        code: ErrorCode,
        /// The words for a log or a person.
        message: String,
    },
}

/// Why the server refused something; the `code` of [`ToClient::Error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The client's protocol version is not the server's.
    ProtocolVersion,
    /// The client sent input faster than the pane consumes it.
    InputBacklog,
    /// The pane does not exist.
    UnknownPane,
    /// No channel number is free.
    ChannelsExhausted,
    /// The client acted on a pane it is not subscribed to.
    NotSubscribed,
}

/// A shell-integration event, fully typed on the wire because the server
/// and the client both act on it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MarkKind {
    /// The shell is about to print a prompt.
    PromptStart,
    /// The user began typing a command.
    CommandStart,
    /// The command started running.
    CommandExecuted,
    /// The command finished.
    CommandFinished {
        /// Its exit status, when the shell reported one.
        exit_status: Option<i32>,
    },
    /// The shell's working directory changed.
    WorkingDirectory {
        /// The directory.
        path: String,
    },
    /// The terminal's title changed.
    Title {
        /// The title.
        text: String,
    },
    /// The alternate screen was entered or left.
    AlternateScreen {
        /// Whether it was entered.
        entered: bool,
    },
}

/// Why a message could not be encoded or decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageError {
    /// A discriminant, error code, mark kind, presence or boolean byte no
    /// variant claims.
    UnknownDiscriminant {
        /// Always [`CHANNEL_CONTROL`], the only channel that is parsed.
        channel: u8,
        /// The byte.
        discriminant: u8,
    },
    /// The bytes end before a field does. For an empty payload the
    /// discriminant reported is 0, the byte that was not there.
    Truncated {
        /// The message's discriminant.
        discriminant: u8,
        /// The bytes the field needed.
        needed: usize,
        /// The bytes that were left.
        available: usize,
    },
    /// Bytes follow the last field.
    TrailingBytes {
        /// The message's discriminant.
        discriminant: u8,
        /// How many.
        count: usize,
    },
    /// A string field is not UTF-8.
    Utf8 {
        /// The message's discriminant.
        discriminant: u8,
    },
    /// Pane output was asked for on the control channel.
    ControlChannel,
    /// The encoding would exceed [`MAXIMUM_PAYLOAD_LENGTH`]; nothing was
    /// allocated for it.
    Oversize {
        /// The length the encoding would have.
        length: usize,
    },
}

impl Display for MessageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            MessageError::UnknownDiscriminant {
                channel,
                discriminant,
            } => write!(
                formatter,
                "byte {discriminant} on channel {channel} names no message, code or kind"
            ),
            MessageError::Truncated {
                discriminant,
                needed,
                available,
            } => write!(
                formatter,
                "message {discriminant} ends early: a field needs {needed} bytes, {available} are left"
            ),
            MessageError::TrailingBytes {
                discriminant,
                count,
            } => write!(
                formatter,
                "message {discriminant} is followed by {count} bytes"
            ),
            MessageError::Utf8 { discriminant } => {
                write!(
                    formatter,
                    "message {discriminant} has a string that is not UTF-8"
                )
            }
            MessageError::ControlChannel => {
                write!(
                    formatter,
                    "channel {CHANNEL_CONTROL} carries control messages, not pane output"
                )
            }
            MessageError::Oversize { length } => write!(
                formatter,
                "an encoding of {length} bytes exceeds the maximum payload of {MAXIMUM_PAYLOAD_LENGTH}"
            ),
        }
    }
}

impl core::error::Error for MessageError {}

/// Where an encoding goes: a byte count first, so an oversize message is
/// refused before anything is allocated, and then a buffer.
trait Sink {
    /// Appends bytes.
    fn put(&mut self, bytes: &[u8]);
}

impl Sink for usize {
    fn put(&mut self, bytes: &[u8]) {
        *self = self.saturating_add(bytes.len());
    }
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// Appends a length-delimited string or payload.
fn put_bytes(sink: &mut dyn Sink, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    sink.put(&length.to_le_bytes());
    sink.put(bytes);
}

/// Appends a message tag and the pane id that follows it.
fn put_pane(sink: &mut dyn Sink, tag: u8, pane: PaneId) {
    sink.put(&[tag]);
    sink.put(&pane.0.to_le_bytes());
}

/// Appends a mark kind: its tag and then its fields.
fn put_mark_kind(sink: &mut dyn Sink, kind: &MarkKind) {
    match kind {
        MarkKind::PromptStart => sink.put(&[mark_tag::PROMPT_START]),
        MarkKind::CommandStart => sink.put(&[mark_tag::COMMAND_START]),
        MarkKind::CommandExecuted => sink.put(&[mark_tag::COMMAND_EXECUTED]),
        MarkKind::CommandFinished { exit_status } => match exit_status {
            None => sink.put(&[mark_tag::COMMAND_FINISHED, ABSENT]),
            Some(status) => {
                sink.put(&[mark_tag::COMMAND_FINISHED, PRESENT]);
                sink.put(&status.to_le_bytes());
            }
        },
        MarkKind::WorkingDirectory { path } => {
            sink.put(&[mark_tag::WORKING_DIRECTORY]);
            put_bytes(sink, path.as_bytes());
        }
        MarkKind::Title { text } => {
            sink.put(&[mark_tag::TITLE]);
            put_bytes(sink, text.as_bytes());
        }
        MarkKind::AlternateScreen { entered } => {
            sink.put(&[
                mark_tag::ALTERNATE_SCREEN,
                if *entered { PRESENT } else { ABSENT },
            ]);
        }
    }
}

impl ErrorCode {
    /// The code's wire value.
    fn tag(self) -> u8 {
        match self {
            ErrorCode::ProtocolVersion => error_tag::PROTOCOL_VERSION,
            ErrorCode::InputBacklog => error_tag::INPUT_BACKLOG,
            ErrorCode::UnknownPane => error_tag::UNKNOWN_PANE,
            ErrorCode::ChannelsExhausted => error_tag::CHANNELS_EXHAUSTED,
            ErrorCode::NotSubscribed => error_tag::NOT_SUBSCRIBED,
        }
    }

    /// The code a wire value names.
    ///
    /// # Errors
    ///
    /// [`MessageError::UnknownDiscriminant`] for a value no code claims.
    fn from_tag(tag: u8) -> Result<ErrorCode, MessageError> {
        match tag {
            error_tag::PROTOCOL_VERSION => Ok(ErrorCode::ProtocolVersion),
            error_tag::INPUT_BACKLOG => Ok(ErrorCode::InputBacklog),
            error_tag::UNKNOWN_PANE => Ok(ErrorCode::UnknownPane),
            error_tag::CHANNELS_EXHAUSTED => Ok(ErrorCode::ChannelsExhausted),
            error_tag::NOT_SUBSCRIBED => Ok(ErrorCode::NotSubscribed),
            other => Err(unknown(other)),
        }
    }
}

/// Appends a client-to-server message.
fn put_to_server(sink: &mut dyn Sink, message: &ToServer) {
    match message {
        ToServer::Hello {
            protocol_version,
            client_version,
            capabilities,
        } => {
            sink.put(&[server_tag::HELLO]);
            sink.put(&protocol_version.to_le_bytes());
            put_bytes(sink, client_version.as_bytes());
            sink.put(&capabilities.bits().to_le_bytes());
        }
        ToServer::SnapshotRequest => sink.put(&[server_tag::SNAPSHOT_REQUEST]),
        ToServer::Command {
            command_id,
            payload,
        } => {
            sink.put(&[server_tag::COMMAND]);
            sink.put(&command_id.0.to_le_bytes());
            put_bytes(sink, payload);
        }
        ToServer::Subscribe { pane } => put_pane(sink, server_tag::SUBSCRIBE, *pane),
        ToServer::Unsubscribe { pane } => put_pane(sink, server_tag::UNSUBSCRIBE, *pane),
        ToServer::Resume {
            pane,
            from_sequence,
        } => {
            put_pane(sink, server_tag::RESUME, *pane);
            sink.put(&from_sequence.0.to_le_bytes());
        }
        ToServer::ScreenRequest { pane } => put_pane(sink, server_tag::SCREEN_REQUEST, *pane),
        ToServer::Credit { channel, bytes } => {
            sink.put(&[server_tag::CREDIT, *channel]);
            sink.put(&bytes.to_le_bytes());
        }
        ToServer::ChannelReleased { channel } => {
            sink.put(&[server_tag::CHANNEL_RELEASED, *channel]);
        }
        ToServer::Input { pane, bytes } => {
            put_pane(sink, server_tag::INPUT, *pane);
            put_bytes(sink, bytes);
        }
        ToServer::Resize {
            pane,
            columns,
            rows,
        } => {
            put_pane(sink, server_tag::RESIZE, *pane);
            sink.put(&columns.to_le_bytes());
            sink.put(&rows.to_le_bytes());
        }
        ToServer::Focus { pane } => put_pane(sink, server_tag::FOCUS, *pane),
        ToServer::Ping => sink.put(&[server_tag::PING]),
    }
}

/// Appends a server-to-client message.
fn put_to_client(sink: &mut dyn Sink, message: &ToClient) {
    match message {
        ToClient::Hello {
            protocol_version,
            server_version,
            capabilities,
        } => {
            sink.put(&[client_tag::HELLO]);
            sink.put(&protocol_version.to_le_bytes());
            put_bytes(sink, server_version.as_bytes());
            sink.put(&capabilities.bits().to_le_bytes());
        }
        ToClient::Snapshot {
            generation,
            payload,
        } => {
            sink.put(&[client_tag::SNAPSHOT]);
            sink.put(&generation.0.to_le_bytes());
            put_bytes(sink, payload);
        }
        ToClient::Delta {
            generation,
            payload,
        } => {
            sink.put(&[client_tag::DELTA]);
            sink.put(&generation.0.to_le_bytes());
            put_bytes(sink, payload);
        }
        ToClient::CommandResult {
            command_id,
            payload,
        } => {
            sink.put(&[client_tag::COMMAND_RESULT]);
            sink.put(&command_id.0.to_le_bytes());
            put_bytes(sink, payload);
        }
        ToClient::PaneChannel {
            pane,
            channel,
            sequence,
        } => {
            put_pane(sink, client_tag::PANE_CHANNEL, *pane);
            sink.put(&[*channel]);
            sink.put(&sequence.0.to_le_bytes());
        }
        ToClient::PaneDetached { pane, channel } => {
            put_pane(sink, client_tag::PANE_DETACHED, *pane);
            sink.put(&[*channel]);
        }
        ToClient::Screen {
            pane,
            sequence,
            columns,
            rows,
            bytes,
        } => {
            put_pane(sink, client_tag::SCREEN, *pane);
            sink.put(&sequence.0.to_le_bytes());
            sink.put(&columns.to_le_bytes());
            sink.put(&rows.to_le_bytes());
            put_bytes(sink, bytes);
        }
        ToClient::Mark {
            pane,
            sequence,
            kind,
        } => {
            put_pane(sink, client_tag::MARK, *pane);
            sink.put(&sequence.0.to_le_bytes());
            put_mark_kind(sink, kind);
        }
        ToClient::Pong => sink.put(&[client_tag::PONG]),
        ToClient::Error { code, message } => {
            sink.put(&[client_tag::ERROR, code.tag()]);
            put_bytes(sink, message.as_bytes());
        }
    }
}

/// Measures an encoding, refuses it if it would not fit a frame, and only
/// then allocates and writes it.
///
/// # Errors
///
/// [`MessageError::Oversize`] when the encoding would exceed
/// [`MAXIMUM_PAYLOAD_LENGTH`].
fn encode(write: impl Fn(&mut dyn Sink)) -> Result<Vec<u8>, MessageError> {
    let mut length: usize = 0;
    write(&mut length);
    let maximum = usize::try_from(MAXIMUM_PAYLOAD_LENGTH).unwrap_or(usize::MAX);
    if length > maximum {
        return Err(MessageError::Oversize { length });
    }
    let mut out = Vec::with_capacity(length);
    write(&mut out);
    Ok(out)
}

/// The bytes of a client-to-server message.
///
/// # Errors
///
/// [`MessageError::Oversize`] when the encoding would exceed
/// [`MAXIMUM_PAYLOAD_LENGTH`], before anything is allocated.
pub fn encode_to_server(message: &ToServer) -> Result<Vec<u8>, MessageError> {
    encode(|sink| put_to_server(sink, message))
}

/// The bytes of a server-to-client message.
///
/// # Errors
///
/// [`MessageError::Oversize`] when the encoding would exceed
/// [`MAXIMUM_PAYLOAD_LENGTH`], before anything is allocated.
pub fn encode_to_client(message: &ToClient) -> Result<Vec<u8>, MessageError> {
    encode(|sink| put_to_client(sink, message))
}

/// A cursor over a message's bytes that remembers the discriminant every
/// refusal names.
struct Reader<'bytes> {
    /// The whole message.
    bytes: &'bytes [u8],
    /// How many bytes have been read.
    position: usize,
    /// The message's discriminant, read first.
    discriminant: u8,
}

impl<'bytes> Reader<'bytes> {
    /// A reader positioned after the discriminant.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] for an empty payload.
    fn new(bytes: &'bytes [u8]) -> Result<Reader<'bytes>, MessageError> {
        let Some(discriminant) = bytes.first() else {
            return Err(MessageError::Truncated {
                discriminant: 0,
                needed: size_of::<u8>(),
                available: 0,
            });
        };
        Ok(Reader {
            bytes,
            position: size_of::<u8>(),
            discriminant: *discriminant,
        })
    }

    /// The next `needed` bytes.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when fewer are left.
    fn take(&mut self, needed: usize) -> Result<&'bytes [u8], MessageError> {
        let available = self.bytes.len().saturating_sub(self.position);
        let end = self.position.saturating_add(needed);
        let Some(taken) = self.bytes.get(self.position..end) else {
            return Err(MessageError::Truncated {
                discriminant: self.discriminant,
                needed,
                available,
            });
        };
        self.position = end;
        Ok(taken)
    }

    /// The next byte.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when none is left.
    fn byte(&mut self) -> Result<u8, MessageError> {
        let [byte] = self.array::<1>()?;
        Ok(byte)
    }

    /// The next `WIDTH` bytes as an array: the little-endian bytes of an
    /// integer, which the caller converts.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when fewer bytes are left.
    fn array<const WIDTH: usize>(&mut self) -> Result<[u8; WIDTH], MessageError> {
        let rest = self.bytes.get(self.position..).unwrap_or_default();
        let Some(chunk) = rest.first_chunk::<WIDTH>() else {
            return Err(MessageError::Truncated {
                discriminant: self.discriminant,
                needed: WIDTH,
                available: rest.len(),
            });
        };
        self.position = self.position.saturating_add(WIDTH);
        Ok(*chunk)
    }

    /// The next length-delimited bytes, owned.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when the length or the bytes are cut short.
    fn bytes(&mut self) -> Result<Vec<u8>, MessageError> {
        let length = u32::from_le_bytes(self.array()?);
        let taken = self.take(usize::try_from(length).unwrap_or(usize::MAX))?;
        Ok(taken.to_vec())
    }

    /// The next length-delimited string.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when it is cut short, [`MessageError::Utf8`]
    /// when it is not UTF-8.
    fn string(&mut self) -> Result<String, MessageError> {
        let discriminant = self.discriminant;
        String::from_utf8(self.bytes()?).map_err(|_error| MessageError::Utf8 { discriminant })
    }

    /// The next byte as a presence or boolean flag.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] when none is left,
    /// [`MessageError::UnknownDiscriminant`] when it is neither 0 nor 1.
    fn flag(&mut self) -> Result<bool, MessageError> {
        match self.byte()? {
            ABSENT => Ok(false),
            PRESENT => Ok(true),
            other => Err(unknown(other)),
        }
    }

    /// Confirms nothing is left.
    ///
    /// # Errors
    ///
    /// [`MessageError::TrailingBytes`] when bytes follow the last field.
    fn finish(self) -> Result<(), MessageError> {
        let count = self.bytes.len().saturating_sub(self.position);
        if count == 0 {
            Ok(())
        } else {
            Err(MessageError::TrailingBytes {
                discriminant: self.discriminant,
                count,
            })
        }
    }
}

/// The refusal of a byte no variant claims.
fn unknown(discriminant: u8) -> MessageError {
    MessageError::UnknownDiscriminant {
        channel: CHANNEL_CONTROL,
        discriminant,
    }
}

/// The next mark kind: its tag and then its fields.
///
/// # Errors
///
/// [`MessageError`] as the fields' readers give it, and
/// [`MessageError::UnknownDiscriminant`] for a tag no kind claims.
fn read_mark_kind(reader: &mut Reader<'_>) -> Result<MarkKind, MessageError> {
    match reader.byte()? {
        mark_tag::PROMPT_START => Ok(MarkKind::PromptStart),
        mark_tag::COMMAND_START => Ok(MarkKind::CommandStart),
        mark_tag::COMMAND_EXECUTED => Ok(MarkKind::CommandExecuted),
        mark_tag::COMMAND_FINISHED => {
            let exit_status = if reader.flag()? {
                Some(i32::from_le_bytes(reader.array()?))
            } else {
                None
            };
            Ok(MarkKind::CommandFinished { exit_status })
        }
        mark_tag::WORKING_DIRECTORY => Ok(MarkKind::WorkingDirectory {
            path: reader.string()?,
        }),
        mark_tag::TITLE => Ok(MarkKind::Title {
            text: reader.string()?,
        }),
        mark_tag::ALTERNATE_SCREEN => Ok(MarkKind::AlternateScreen {
            entered: reader.flag()?,
        }),
        other => Err(unknown(other)),
    }
}

/// The client-to-server message a control payload holds.
///
/// # Errors
///
/// [`MessageError::UnknownDiscriminant`], [`MessageError::Truncated`],
/// [`MessageError::TrailingBytes`] or [`MessageError::Utf8`], each naming
/// what it found.
pub fn decode_to_server(payload: &[u8]) -> Result<ToServer, MessageError> {
    let mut reader = Reader::new(payload)?;
    let message = match reader.discriminant {
        server_tag::HELLO => ToServer::Hello {
            protocol_version: u16::from_le_bytes(reader.array()?),
            client_version: reader.string()?,
            capabilities: Capabilities::from_bits(u32::from_le_bytes(reader.array()?)),
        },
        server_tag::SNAPSHOT_REQUEST => ToServer::SnapshotRequest,
        server_tag::COMMAND => ToServer::Command {
            command_id: CommandId(u64::from_le_bytes(reader.array()?)),
            payload: reader.bytes()?,
        },
        server_tag::SUBSCRIBE => ToServer::Subscribe {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
        },
        server_tag::UNSUBSCRIBE => ToServer::Unsubscribe {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
        },
        server_tag::RESUME => ToServer::Resume {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            from_sequence: Sequence(u64::from_le_bytes(reader.array()?)),
        },
        server_tag::SCREEN_REQUEST => ToServer::ScreenRequest {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
        },
        server_tag::CREDIT => ToServer::Credit {
            channel: reader.byte()?,
            bytes: u32::from_le_bytes(reader.array()?),
        },
        server_tag::CHANNEL_RELEASED => ToServer::ChannelReleased {
            channel: reader.byte()?,
        },
        server_tag::INPUT => ToServer::Input {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            bytes: reader.bytes()?,
        },
        server_tag::RESIZE => ToServer::Resize {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            columns: u16::from_le_bytes(reader.array()?),
            rows: u16::from_le_bytes(reader.array()?),
        },
        server_tag::FOCUS => ToServer::Focus {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
        },
        server_tag::PING => ToServer::Ping,
        other => return Err(unknown(other)),
    };
    reader.finish()?;
    Ok(message)
}

/// The server-to-client message a control payload holds.
///
/// # Errors
///
/// [`MessageError::UnknownDiscriminant`], [`MessageError::Truncated`],
/// [`MessageError::TrailingBytes`] or [`MessageError::Utf8`], each naming
/// what it found.
pub fn decode_to_client(payload: &[u8]) -> Result<ToClient, MessageError> {
    let mut reader = Reader::new(payload)?;
    let message = match reader.discriminant {
        client_tag::HELLO => ToClient::Hello {
            protocol_version: u16::from_le_bytes(reader.array()?),
            server_version: reader.string()?,
            capabilities: Capabilities::from_bits(u32::from_le_bytes(reader.array()?)),
        },
        client_tag::SNAPSHOT => ToClient::Snapshot {
            generation: Generation(u64::from_le_bytes(reader.array()?)),
            payload: reader.bytes()?,
        },
        client_tag::DELTA => ToClient::Delta {
            generation: Generation(u64::from_le_bytes(reader.array()?)),
            payload: reader.bytes()?,
        },
        client_tag::COMMAND_RESULT => ToClient::CommandResult {
            command_id: CommandId(u64::from_le_bytes(reader.array()?)),
            payload: reader.bytes()?,
        },
        client_tag::PANE_CHANNEL => ToClient::PaneChannel {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            channel: reader.byte()?,
            sequence: Sequence(u64::from_le_bytes(reader.array()?)),
        },
        client_tag::PANE_DETACHED => ToClient::PaneDetached {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            channel: reader.byte()?,
        },
        client_tag::SCREEN => ToClient::Screen {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            sequence: Sequence(u64::from_le_bytes(reader.array()?)),
            columns: u16::from_le_bytes(reader.array()?),
            rows: u16::from_le_bytes(reader.array()?),
            bytes: reader.bytes()?,
        },
        client_tag::MARK => ToClient::Mark {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            sequence: Sequence(u64::from_le_bytes(reader.array()?)),
            kind: read_mark_kind(&mut reader)?,
        },
        client_tag::PONG => ToClient::Pong,
        client_tag::ERROR => ToClient::Error {
            code: ErrorCode::from_tag(reader.byte()?)?,
            message: reader.string()?,
        },
        other => return Err(unknown(other)),
    };
    reader.finish()?;
    Ok(message)
}

/// Pane output: the payload itself, borrowed, never parsed and never copied.
///
/// # Errors
///
/// [`MessageError::ControlChannel`] for channel 0, which carries control
/// messages.
pub fn pane_output(channel: u8, payload: &[u8]) -> Result<&[u8], MessageError> {
    if channel == CHANNEL_CONTROL {
        return Err(MessageError::ControlChannel);
    }
    Ok(payload)
}
