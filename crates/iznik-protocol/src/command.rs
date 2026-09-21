//! The session commands, their outcomes and rejection codes, encoded as the
//! `Command` and `CommandResult` payloads.
//!
//! Every command names stable identity, never a position: a tab is closed by
//! its id, and a client that closes "the third tab" has already lost the race
//! with the client that closed the first. The one thing named by position is
//! where a new pane goes, and [`Placement`] says that against a pane id too —
//! a new pane replaces the target's leaf with a split of the two, weights
//! equal, in the direction given, the new pane before or after the target.
//!
//! Every command is answered exactly once with a [`CommandOutcome`]: what it
//! created, or the [`RejectionCode`] that says what was wrong. A rejected
//! command changes nothing, so a client can apply the answer without asking
//! what else may have happened.
//!
//! On the wire a command and an outcome are each a one-byte discriminant and
//! then their fields in declaration order, in the forms [`crate::model`]
//! documents. The golden `tests/fixtures/command.jsonl` pins every byte.

use crate::capabilities::Capabilities;
use crate::identity::{Generation, PaneId, SessionId, TabId};
use crate::message::MessageError;
use crate::model::{LayoutNode, SplitDirection, check_depth, put_layout, read_layout};
use crate::wire::{Reader, Sink, encode, put_bytes, put_count, put_optional, unknown};

/// The depth the outermost node of a layout sits at.
const ROOT_DEPTH: usize = 1;

/// The discriminants of [`SessionCommand`], in declaration order.
mod command_tag {
    /// `CreateSession`.
    pub(super) const CREATE_SESSION: u8 = 0;
    /// `RenameSession`.
    pub(super) const RENAME_SESSION: u8 = 1;
    /// `CloseSession`.
    pub(super) const CLOSE_SESSION: u8 = 2;
    /// `CreateTab`.
    pub(super) const CREATE_TAB: u8 = 3;
    /// `RenameTab`.
    pub(super) const RENAME_TAB: u8 = 4;
    /// `CloseTab`.
    pub(super) const CLOSE_TAB: u8 = 5;
    /// `ReorderTabs`.
    pub(super) const REORDER_TABS: u8 = 6;
    /// `CreatePane`.
    pub(super) const CREATE_PANE: u8 = 7;
    /// `ClosePane`.
    pub(super) const CLOSE_PANE: u8 = 8;
    /// `MovePane`.
    pub(super) const MOVE_PANE: u8 = 9;
    /// `SetLayout`.
    pub(super) const SET_LAYOUT: u8 = 10;
    /// `ReorderSessions`.
    pub(super) const REORDER_SESSIONS: u8 = 11;
}

/// The discriminants of [`CommandOutcome`], in declaration order.
mod outcome_tag {
    /// `Applied`.
    pub(super) const APPLIED: u8 = 0;
    /// `Rejected`.
    pub(super) const REJECTED: u8 = 1;
}

/// The wire values of [`Created`], in declaration order.
mod created_tag {
    /// `Nothing`.
    pub(super) const NOTHING: u8 = 0;
    /// `Session`.
    pub(super) const SESSION: u8 = 1;
    /// `Tab`.
    pub(super) const TAB: u8 = 2;
    /// `Pane`.
    pub(super) const PANE: u8 = 3;
}

/// The wire values of [`RejectionCode`], in declaration order.
mod rejection_tag {
    /// `UnknownSession`.
    pub(super) const UNKNOWN_SESSION: u8 = 0;
    /// `UnknownTab`.
    pub(super) const UNKNOWN_TAB: u8 = 1;
    /// `UnknownPane`.
    pub(super) const UNKNOWN_PANE: u8 = 2;
    /// `EmptyName`.
    pub(super) const EMPTY_NAME: u8 = 3;
    /// `InvalidOrder`.
    pub(super) const INVALID_ORDER: u8 = 4;
    /// `InvalidLayout`.
    pub(super) const INVALID_LAYOUT: u8 = 5;
    /// `SpawnFailed`.
    pub(super) const SPAWN_FAILED: u8 = 6;
    /// `UnknownCommand`.
    pub(super) const UNKNOWN_COMMAND: u8 = 7;
}

/// Where a new pane goes: beside a pane that is already there.
///
/// The target's leaf is replaced by a split of the two, weights equal, in the
/// direction given, the new pane before or after the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Placement {
    /// The pane the new one is placed beside.
    pub target: PaneId,
    /// Which way the two divide the space the target had.
    pub direction: SplitDirection,
    /// Whether the new pane comes before the target rather than after it.
    pub before: bool,
}

/// What a client asks the host to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    /// Make a session, with its first tab and that tab's first pane.
    CreateSession {
        /// What to call it.
        name: String,
        /// The first pane's width in cells.
        columns: u16,
        /// The first pane's height in cells.
        rows: u16,
        /// Where the first pane's shell starts, when it is not the default.
        working_directory: Option<String>,
    },
    /// Rename a session.
    RenameSession {
        /// Which one.
        session: SessionId,
        /// What to call it.
        name: String,
    },
    /// Close a session and everything under it.
    CloseSession {
        /// Which one.
        session: SessionId,
    },
    /// Make a tab in a session, with its first pane.
    CreateTab {
        /// Which session.
        session: SessionId,
        /// What to call it.
        name: String,
        /// The first pane's width in cells.
        columns: u16,
        /// The first pane's height in cells.
        rows: u16,
        /// Where the first pane's shell starts, when it is not the default.
        working_directory: Option<String>,
    },
    /// Rename a tab.
    RenameTab {
        /// Which one.
        tab: TabId,
        /// What to call it.
        name: String,
    },
    /// Close a tab and every pane in it.
    CloseTab {
        /// Which one.
        tab: TabId,
    },
    /// Put a session's tabs in another order, carried whole.
    ReorderTabs {
        /// Which session.
        session: SessionId,
        /// The whole order, never a swap.
        order: Vec<TabId>,
    },
    /// Make a pane in a tab, beside one that is there.
    CreatePane {
        /// Which tab.
        tab: TabId,
        /// Where it goes.
        placement: Placement,
        /// Its width in cells.
        columns: u16,
        /// Its height in cells.
        rows: u16,
        /// Where its shell starts, when it is not the default.
        working_directory: Option<String>,
    },
    /// Close a pane.
    ClosePane {
        /// Which one.
        pane: PaneId,
    },
    /// Move a pane into another tab, beside one that is there.
    MovePane {
        /// Which pane.
        pane: PaneId,
        /// Which tab it goes to.
        to_tab: TabId,
        /// Where in that tab it goes.
        placement: Placement,
    },
    /// Arrange a tab another way.
    SetLayout {
        /// Which tab.
        tab: TabId,
        /// The whole arrangement; the host normalizes it.
        layout: LayoutNode,
    },
    /// Put the host's sessions in another order, carried whole.
    ReorderSessions {
        /// The whole order, never a swap.
        order: Vec<SessionId>,
    },
}

impl SessionCommand {
    /// The command's name, as a person and a log read it.
    ///
    /// The protocol's own spelling of the variant, so a refusal names the
    /// command the same way wherever it is printed.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            SessionCommand::CreateSession { .. } => "CreateSession",
            SessionCommand::RenameSession { .. } => "RenameSession",
            SessionCommand::CloseSession { .. } => "CloseSession",
            SessionCommand::ReorderSessions { .. } => "ReorderSessions",
            SessionCommand::CreateTab { .. } => "CreateTab",
            SessionCommand::RenameTab { .. } => "RenameTab",
            SessionCommand::CloseTab { .. } => "CloseTab",
            SessionCommand::ReorderTabs { .. } => "ReorderTabs",
            SessionCommand::CreatePane { .. } => "CreatePane",
            SessionCommand::ClosePane { .. } => "ClosePane",
            SessionCommand::MovePane { .. } => "MovePane",
            SessionCommand::SetLayout { .. } => "SetLayout",
        }
    }

    /// What a server must advertise before it can decode this command.
    ///
    /// Every command but [`SessionCommand::ReorderSessions`] has been in the
    /// protocol since it had one, so a server that connects at all decodes it;
    /// that one arrived later and is advertised as
    /// [`Capabilities::REORDER_SESSIONS`]. A client sends a command only when
    /// the server it is connected to set the bit, because a server without it
    /// refuses the frame as garbage and the connection dies with it.
    #[must_use]
    pub fn needs(&self) -> Capabilities {
        match self {
            SessionCommand::ReorderSessions { .. } => Capabilities::REORDER_SESSIONS,
            SessionCommand::CreateSession { .. }
            | SessionCommand::RenameSession { .. }
            | SessionCommand::CloseSession { .. }
            | SessionCommand::CreateTab { .. }
            | SessionCommand::RenameTab { .. }
            | SessionCommand::CloseTab { .. }
            | SessionCommand::ReorderTabs { .. }
            | SessionCommand::CreatePane { .. }
            | SessionCommand::ClosePane { .. }
            | SessionCommand::MovePane { .. }
            | SessionCommand::SetLayout { .. } => Capabilities::from_bits(0),
        }
    }

    /// Whether a server advertising `advertised` can decode this command.
    #[must_use]
    pub fn is_supported_by(&self, advertised: Capabilities) -> bool {
        let needed = self.needs();
        advertised.bits() & needed.bits() == needed.bits()
    }
}

/// The answer to a command, given exactly once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    /// It was applied.
    Applied {
        /// The generation the model reached.
        generation: Generation,
        /// What it made, if anything.
        created: Created,
    },
    /// It was refused, and nothing changed.
    Rejected {
        /// What was wrong.
        code: RejectionCode,
        /// The words for a log or a person.
        message: String,
    },
}

/// What a command made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Created {
    /// Nothing: the command changed something that was already there.
    Nothing,
    /// A session, and the tab and pane under it.
    Session(SessionId),
    /// A tab, and the pane in it.
    Tab(TabId),
    /// A pane.
    Pane(PaneId),
}

/// Why a command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectionCode {
    /// It named a session the host does not hold.
    UnknownSession,
    /// It named a tab the host does not hold.
    UnknownTab,
    /// It named a pane the host does not hold.
    UnknownPane,
    /// It gave an empty name, which no session or tab may carry.
    EmptyName,
    /// Its order is not a permutation of the session's tabs.
    InvalidOrder,
    /// Its layout does not place exactly the tab's panes, each once, or nests
    /// deeper than a model holds.
    InvalidLayout,
    /// A pseudoterminal or its child could not be started.
    SpawnFailed,
    /// The command's tag is one this server does not know.
    ///
    /// Two peers of one protocol version are not necessarily one build — a
    /// released server and a local one both say "protocol 1" — so a command
    /// this codec cannot read is a request the server cannot serve, not a peer
    /// speaking garbage. It is refused, and the connection lives: the panes on
    /// it are somebody's sessions, and losing them over one unknown tag is the
    /// bug this code exists to prevent.
    UnknownCommand,
}

impl RejectionCode {
    /// The code's wire value.
    fn tag(self) -> u8 {
        match self {
            RejectionCode::UnknownSession => rejection_tag::UNKNOWN_SESSION,
            RejectionCode::UnknownTab => rejection_tag::UNKNOWN_TAB,
            RejectionCode::UnknownPane => rejection_tag::UNKNOWN_PANE,
            RejectionCode::EmptyName => rejection_tag::EMPTY_NAME,
            RejectionCode::InvalidOrder => rejection_tag::INVALID_ORDER,
            RejectionCode::InvalidLayout => rejection_tag::INVALID_LAYOUT,
            RejectionCode::SpawnFailed => rejection_tag::SPAWN_FAILED,
            RejectionCode::UnknownCommand => rejection_tag::UNKNOWN_COMMAND,
        }
    }

    /// The code a wire value names.
    ///
    /// # Errors
    ///
    /// [`MessageError::UnknownDiscriminant`] for a value no code claims.
    fn from_tag(tag: u8) -> Result<RejectionCode, MessageError> {
        match tag {
            rejection_tag::UNKNOWN_SESSION => Ok(RejectionCode::UnknownSession),
            rejection_tag::UNKNOWN_TAB => Ok(RejectionCode::UnknownTab),
            rejection_tag::UNKNOWN_PANE => Ok(RejectionCode::UnknownPane),
            rejection_tag::EMPTY_NAME => Ok(RejectionCode::EmptyName),
            rejection_tag::INVALID_ORDER => Ok(RejectionCode::InvalidOrder),
            rejection_tag::INVALID_LAYOUT => Ok(RejectionCode::InvalidLayout),
            rejection_tag::SPAWN_FAILED => Ok(RejectionCode::SpawnFailed),
            rejection_tag::UNKNOWN_COMMAND => Ok(RejectionCode::UnknownCommand),
            other => Err(unknown(other)),
        }
    }
}

/// Appends a placement: the pane it is beside, the direction, and the side.
fn put_placement(sink: &mut dyn Sink, placement: Placement) {
    sink.put(&placement.target.0.to_le_bytes());
    sink.put(&[placement.direction.tag(), u8::from(placement.before)]);
}

/// The placement at the reader.
///
/// # Errors
///
/// The refusals [`decode_session_command`] documents.
fn read_placement(reader: &mut Reader<'_>) -> Result<Placement, MessageError> {
    Ok(Placement {
        target: PaneId(u64::from_le_bytes(reader.array()?)),
        direction: SplitDirection::from_tag(reader.byte()?)?,
        before: reader.flag()?,
    })
}

/// Appends the size and working directory a creating command ends with.
fn put_start(sink: &mut dyn Sink, columns: u16, rows: u16, directory: Option<&str>) {
    sink.put(&columns.to_le_bytes());
    sink.put(&rows.to_le_bytes());
    put_optional(sink, directory);
}

/// Appends a command: its discriminant and then its fields.
///
/// Neither half below knows every one of them — one match of that many arms
/// and their fields is longer than a function may be — so each says whether it
/// wrote what it was given, and this offers it to the other when it did not. A
/// variant moved between the two is therefore written by the half that knows
/// it rather than by neither, and `check_layouts` is exhaustive, so a new one
/// is a compile error there rather than a command that encodes to nothing.
fn put_command(sink: &mut dyn Sink, command: &SessionCommand) {
    if !put_arrangement(sink, command) {
        let _written = put_pane_command(sink, command);
    }
}

/// Refuses a command carrying a layout nested past what a model holds,
/// exhaustively, so a later command that carries one cannot slip past.
///
/// # Errors
///
/// [`MessageError::LayoutTooDeep`], naming the bound.
fn check_layouts(command: &SessionCommand) -> Result<(), MessageError> {
    match command {
        SessionCommand::SetLayout { layout, .. } => check_depth(layout),
        SessionCommand::CreateSession { .. }
        | SessionCommand::RenameSession { .. }
        | SessionCommand::CloseSession { .. }
        | SessionCommand::CreateTab { .. }
        | SessionCommand::RenameTab { .. }
        | SessionCommand::CloseTab { .. }
        | SessionCommand::ReorderTabs { .. }
        | SessionCommand::ReorderSessions { .. }
        | SessionCommand::CreatePane { .. }
        | SessionCommand::ClosePane { .. }
        | SessionCommand::MovePane { .. } => Ok(()),
    }
}

/// Appends the commands that name a session or a tab.
fn put_arrangement(sink: &mut dyn Sink, command: &SessionCommand) -> bool {
    match command {
        SessionCommand::CreateSession {
            name,
            columns,
            rows,
            working_directory,
        } => {
            sink.put(&[command_tag::CREATE_SESSION]);
            put_bytes(sink, name.as_bytes());
            put_start(sink, *columns, *rows, working_directory.as_deref());
        }
        SessionCommand::RenameSession { session, name } => {
            sink.put(&[command_tag::RENAME_SESSION]);
            sink.put(&session.0.to_le_bytes());
            put_bytes(sink, name.as_bytes());
        }
        SessionCommand::CloseSession { session } => {
            sink.put(&[command_tag::CLOSE_SESSION]);
            sink.put(&session.0.to_le_bytes());
        }
        SessionCommand::CreateTab {
            session,
            name,
            columns,
            rows,
            working_directory,
        } => {
            sink.put(&[command_tag::CREATE_TAB]);
            sink.put(&session.0.to_le_bytes());
            put_bytes(sink, name.as_bytes());
            put_start(sink, *columns, *rows, working_directory.as_deref());
        }
        SessionCommand::RenameTab { tab, name } => {
            sink.put(&[command_tag::RENAME_TAB]);
            sink.put(&tab.0.to_le_bytes());
            put_bytes(sink, name.as_bytes());
        }
        SessionCommand::CloseTab { tab } => {
            sink.put(&[command_tag::CLOSE_TAB]);
            sink.put(&tab.0.to_le_bytes());
        }
        SessionCommand::ReorderTabs { session, order } => {
            sink.put(&[command_tag::REORDER_TABS]);
            sink.put(&session.0.to_le_bytes());
            put_count(sink, order.len());
            for tab in order {
                sink.put(&tab.0.to_le_bytes());
            }
        }
        SessionCommand::SetLayout { tab, layout } => {
            sink.put(&[command_tag::SET_LAYOUT]);
            sink.put(&tab.0.to_le_bytes());
            put_layout(sink, layout);
        }
        SessionCommand::ReorderSessions { order } => {
            sink.put(&[command_tag::REORDER_SESSIONS]);
            put_count(sink, order.len());
            for session in order {
                sink.put(&session.0.to_le_bytes());
            }
        }
        _other => return false,
    }
    true
}

/// Appends the commands that name a pane.
fn put_pane_command(sink: &mut dyn Sink, command: &SessionCommand) -> bool {
    match command {
        SessionCommand::CreatePane {
            tab,
            placement,
            columns,
            rows,
            working_directory,
        } => {
            sink.put(&[command_tag::CREATE_PANE]);
            sink.put(&tab.0.to_le_bytes());
            put_placement(sink, *placement);
            put_start(sink, *columns, *rows, working_directory.as_deref());
        }
        SessionCommand::ClosePane { pane } => {
            sink.put(&[command_tag::CLOSE_PANE]);
            sink.put(&pane.0.to_le_bytes());
        }
        SessionCommand::MovePane {
            pane,
            to_tab,
            placement,
        } => {
            sink.put(&[command_tag::MOVE_PANE]);
            sink.put(&pane.0.to_le_bytes());
            sink.put(&to_tab.0.to_le_bytes());
            put_placement(sink, *placement);
        }
        _other => return false,
    }
    true
}

/// The `Command` payload for a command.
///
/// # Errors
///
/// [`MessageError::LayoutTooDeep`] when a layout the command carries nests
/// past [`crate::model::MAXIMUM_LAYOUT_DEPTH`], so nothing this encoder
/// produces is something [`decode_session_command`] refuses, and
/// [`MessageError::Oversize`] when the encoding would not fit a frame.
pub fn encode_session_command(command: &SessionCommand) -> Result<Vec<u8>, MessageError> {
    check_layouts(command)?;
    encode(|sink| put_command(sink, command))
}

/// The command a `Command` payload holds.
///
/// # Errors
///
/// [`MessageError::UnknownDiscriminant`] for a command, split direction,
/// layout tag or presence byte no variant claims;
/// [`MessageError::Truncated`] when a field ends early;
/// [`MessageError::TrailingBytes`] when bytes follow the last field;
/// [`MessageError::Utf8`] when a name or a directory is not UTF-8; and
/// [`MessageError::LayoutTooDeep`] for a layout nested past what a model
/// holds. Every refusal names the command whose field ran out.
pub fn decode_session_command(bytes: &[u8]) -> Result<SessionCommand, MessageError> {
    let mut reader = Reader::new(bytes)?;
    let command = read_arrangement(&mut reader)?;
    reader.finish()?;
    Ok(command)
}

/// The command at the reader, whose discriminant it has already read.
///
/// # Errors
///
/// The refusals [`decode_session_command`] documents.
fn read_arrangement(reader: &mut Reader<'_>) -> Result<SessionCommand, MessageError> {
    match reader.discriminant {
        command_tag::CREATE_SESSION => Ok(SessionCommand::CreateSession {
            name: reader.string()?,
            columns: u16::from_le_bytes(reader.array()?),
            rows: u16::from_le_bytes(reader.array()?),
            working_directory: reader.optional_string()?,
        }),
        command_tag::RENAME_SESSION => Ok(SessionCommand::RenameSession {
            session: SessionId(u64::from_le_bytes(reader.array()?)),
            name: reader.string()?,
        }),
        command_tag::CLOSE_SESSION => Ok(SessionCommand::CloseSession {
            session: SessionId(u64::from_le_bytes(reader.array()?)),
        }),
        command_tag::CREATE_TAB => Ok(SessionCommand::CreateTab {
            session: SessionId(u64::from_le_bytes(reader.array()?)),
            name: reader.string()?,
            columns: u16::from_le_bytes(reader.array()?),
            rows: u16::from_le_bytes(reader.array()?),
            working_directory: reader.optional_string()?,
        }),
        command_tag::RENAME_TAB => Ok(SessionCommand::RenameTab {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
            name: reader.string()?,
        }),
        command_tag::CLOSE_TAB => Ok(SessionCommand::CloseTab {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
        }),
        command_tag::REORDER_TABS => read_reorder(reader),
        command_tag::SET_LAYOUT => Ok(SessionCommand::SetLayout {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
            layout: read_layout(reader, ROOT_DEPTH)?,
        }),
        command_tag::REORDER_SESSIONS => read_session_order(reader),
        _other => read_pane_command(reader),
    }
}

/// The tab reorder at the reader, whose whole order follows its count.
///
/// # Errors
///
/// The refusals [`decode_session_command`] documents.
fn read_reorder(reader: &mut Reader<'_>) -> Result<SessionCommand, MessageError> {
    let session = SessionId(u64::from_le_bytes(reader.array()?));
    let count = reader.count()?;
    let mut order = Vec::new();
    for _index in 0..count {
        order.push(TabId(u64::from_le_bytes(reader.array()?)));
    }
    Ok(SessionCommand::ReorderTabs { session, order })
}

/// The session reorder at the reader, whose whole order follows its count.
///
/// # Errors
///
/// The refusals [`decode_session_command`] documents.
fn read_session_order(reader: &mut Reader<'_>) -> Result<SessionCommand, MessageError> {
    let count = reader.count()?;
    let mut order = Vec::new();
    for _index in 0..count {
        order.push(SessionId(u64::from_le_bytes(reader.array()?)));
    }
    Ok(SessionCommand::ReorderSessions { order })
}

/// The pane command at the reader, whose discriminant it has already read.
///
/// # Errors
///
/// The refusals [`decode_session_command`] documents.
fn read_pane_command(reader: &mut Reader<'_>) -> Result<SessionCommand, MessageError> {
    match reader.discriminant {
        command_tag::CREATE_PANE => Ok(SessionCommand::CreatePane {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
            placement: read_placement(reader)?,
            columns: u16::from_le_bytes(reader.array()?),
            rows: u16::from_le_bytes(reader.array()?),
            working_directory: reader.optional_string()?,
        }),
        command_tag::CLOSE_PANE => Ok(SessionCommand::ClosePane {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
        }),
        command_tag::MOVE_PANE => Ok(SessionCommand::MovePane {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            to_tab: TabId(u64::from_le_bytes(reader.array()?)),
            placement: read_placement(reader)?,
        }),
        other => Err(unknown(other)),
    }
}

/// The `CommandResult` payload for an outcome.
///
/// # Errors
///
/// [`MessageError::Oversize`] when the encoding would not fit a frame,
/// measured before anything is allocated for it.
pub fn encode_command_outcome(outcome: &CommandOutcome) -> Result<Vec<u8>, MessageError> {
    encode(|sink| match outcome {
        CommandOutcome::Applied {
            generation,
            created,
        } => {
            sink.put(&[outcome_tag::APPLIED]);
            sink.put(&generation.0.to_le_bytes());
            match created {
                Created::Nothing => sink.put(&[created_tag::NOTHING]),
                Created::Session(session) => {
                    sink.put(&[created_tag::SESSION]);
                    sink.put(&session.0.to_le_bytes());
                }
                Created::Tab(tab) => {
                    sink.put(&[created_tag::TAB]);
                    sink.put(&tab.0.to_le_bytes());
                }
                Created::Pane(pane) => {
                    sink.put(&[created_tag::PANE]);
                    sink.put(&pane.0.to_le_bytes());
                }
            }
        }
        CommandOutcome::Rejected { code, message } => {
            sink.put(&[outcome_tag::REJECTED, code.tag()]);
            put_bytes(sink, message.as_bytes());
        }
    })
}

/// The outcome a `CommandResult` payload holds.
///
/// # Errors
///
/// [`MessageError::UnknownDiscriminant`] for an outcome, created form or
/// rejection code no variant claims; [`MessageError::Truncated`] when a field
/// ends early; [`MessageError::TrailingBytes`] when bytes follow the last
/// field; and [`MessageError::Utf8`] when the message is not UTF-8.
pub fn decode_command_outcome(bytes: &[u8]) -> Result<CommandOutcome, MessageError> {
    let mut reader = Reader::new(bytes)?;
    let outcome = match reader.discriminant {
        outcome_tag::APPLIED => CommandOutcome::Applied {
            generation: Generation(u64::from_le_bytes(reader.array()?)),
            created: read_created(&mut reader)?,
        },
        outcome_tag::REJECTED => CommandOutcome::Rejected {
            code: RejectionCode::from_tag(reader.byte()?)?,
            message: reader.string()?,
        },
        other => return Err(unknown(other)),
    };
    reader.finish()?;
    Ok(outcome)
}

/// What a command made, at the reader.
///
/// # Errors
///
/// The refusals [`decode_command_outcome`] documents.
fn read_created(reader: &mut Reader<'_>) -> Result<Created, MessageError> {
    match reader.byte()? {
        created_tag::NOTHING => Ok(Created::Nothing),
        created_tag::SESSION => Ok(Created::Session(SessionId(u64::from_le_bytes(
            reader.array()?,
        )))),
        created_tag::TAB => Ok(Created::Tab(TabId(u64::from_le_bytes(reader.array()?)))),
        created_tag::PANE => Ok(Created::Pane(PaneId(u64::from_le_bytes(reader.array()?)))),
        other => Err(unknown(other)),
    }
}
