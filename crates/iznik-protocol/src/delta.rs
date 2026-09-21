//! The numbered deltas, each the smallest thing that can happen to the host
//! model, encoded as the `Delta` payload.
//!
//! Each variant is the smallest thing that can happen: anything larger is
//! several of these, and anything that cannot be expressed as some of these is
//! a snapshot. That is why a pane appearing and the layout that places it are
//! two deltas rather than one — and why the model between them is a model
//! mid-change, which [`crate::reconcile::apply`] applies rather than refuses.
//!
//! [`Delta::TabsReordered`] carries the whole order, never a swap: a swap
//! applied to an arrangement other than the one it was computed against
//! silently produces a third arrangement nobody has.
//!
//! On the wire a delta is a one-byte discriminant and then its fields in
//! declaration order, in the forms [`crate::model`] documents; an index is
//! written the width a count is, and an exit status is a tag and a signed
//! four-byte value, so a signal death and a status of the same number are not
//! the same bytes. The golden `tests/fixtures/delta.jsonl` pins every byte.

use crate::identity::{PaneId, SessionId, TabId};
use crate::message::MessageError;
use crate::model::{
    LayoutNode, Pane, Session, Tab, check_depth, put_layout, put_pane, put_session, put_tab,
    read_layout, read_pane, read_session, read_tab,
};
use crate::wire::{Reader, Sink, encode, put_bytes, put_count, unknown};

/// The depth the outermost node of a layout sits at.
const ROOT_DEPTH: usize = 1;

/// The discriminants of [`Delta`], in declaration order.
mod delta_tag {
    /// `SessionAdded`.
    pub(super) const SESSION_ADDED: u8 = 0;
    /// `SessionRenamed`.
    pub(super) const SESSION_RENAMED: u8 = 1;
    /// `SessionRemoved`.
    pub(super) const SESSION_REMOVED: u8 = 2;
    /// `TabAdded`.
    pub(super) const TAB_ADDED: u8 = 3;
    /// `TabRenamed`.
    pub(super) const TAB_RENAMED: u8 = 4;
    /// `TabRemoved`.
    pub(super) const TAB_REMOVED: u8 = 5;
    /// `TabsReordered`.
    pub(super) const TABS_REORDERED: u8 = 6;
    /// `PaneAdded`.
    pub(super) const PANE_ADDED: u8 = 7;
    /// `PaneRemoved`.
    pub(super) const PANE_REMOVED: u8 = 8;
    /// `PaneMoved`.
    pub(super) const PANE_MOVED: u8 = 9;
    /// `LayoutChanged`.
    pub(super) const LAYOUT_CHANGED: u8 = 10;
    /// `PaneTitle`.
    pub(super) const PANE_TITLE: u8 = 11;
    /// `PaneWorkingDirectory`.
    pub(super) const PANE_WORKING_DIRECTORY: u8 = 12;
    /// `PaneResized`.
    pub(super) const PANE_RESIZED: u8 = 13;
    /// `SessionsReordered`.
    pub(super) const SESSIONS_REORDERED: u8 = 14;
}

/// The wire values of [`RemovalReason`], in declaration order.
mod reason_tag {
    /// `Closed`.
    pub(super) const CLOSED: u8 = 0;
    /// `Exited`.
    pub(super) const EXITED: u8 = 1;
}

/// The wire values of [`ExitStatus`], in declaration order.
mod status_tag {
    /// `Exited`.
    pub(super) const EXITED: u8 = 0;
    /// `Signalled`.
    pub(super) const SIGNALLED: u8 = 1;
}

/// One change to the host model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delta {
    /// A session appeared, with its first tab and that tab's first pane.
    SessionAdded {
        /// The whole session, as it now is.
        session: Session,
    },
    /// A session was renamed.
    SessionRenamed {
        /// The session.
        session: SessionId,
        /// Its new name.
        name: String,
    },
    /// A session and everything under it is gone.
    SessionRemoved {
        /// The session.
        session: SessionId,
    },
    /// A tab appeared, with its first pane.
    TabAdded {
        /// The session it joined.
        session: SessionId,
        /// The whole tab, as it now is.
        tab: Tab,
        /// Where in the session's tabs it sits.
        index: usize,
    },
    /// A tab was renamed.
    TabRenamed {
        /// The tab.
        tab: TabId,
        /// Its new name.
        name: String,
    },
    /// A tab and everything in it is gone.
    TabRemoved {
        /// The tab.
        tab: TabId,
    },
    /// A session's tabs are in another order.
    TabsReordered {
        /// The session.
        session: SessionId,
        /// The whole order, never a swap.
        order: Vec<TabId>,
    },
    /// A pane appeared in a tab. The layout that places it is the
    /// [`Delta::LayoutChanged`] that follows.
    PaneAdded {
        /// The tab it joined.
        tab: TabId,
        /// The whole pane, as it now is.
        pane: Pane,
    },
    /// A pane is gone. The layout that no longer places it is the
    /// [`Delta::LayoutChanged`] that follows, unless the tab went with it.
    PaneRemoved {
        /// The pane.
        pane: PaneId,
        /// Why it went.
        reason: RemovalReason,
    },
    /// A pane belongs to another tab now.
    PaneMoved {
        /// The pane.
        pane: PaneId,
        /// The tab it belongs to now.
        to_tab: TabId,
    },
    /// A tab is arranged another way.
    LayoutChanged {
        /// The tab.
        tab: TabId,
        /// The whole arrangement, normalized when it is applied.
        layout: LayoutNode,
    },
    /// A pane's title changed.
    PaneTitle {
        /// The pane.
        pane: PaneId,
        /// What the program running in it now calls itself.
        title: String,
    },
    /// A pane's shell reported where it is.
    PaneWorkingDirectory {
        /// The pane.
        pane: PaneId,
        /// The directory.
        path: String,
    },
    /// A pane's size changed.
    PaneResized {
        /// The pane.
        pane: PaneId,
        /// Its width in cells.
        columns: u16,
        /// Its height in cells.
        rows: u16,
    },
    /// The host's sessions are in another order.
    SessionsReordered {
        /// The whole order, never a swap.
        order: Vec<SessionId>,
    },
}

/// Why a pane is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemovalReason {
    /// A client closed it.
    Closed,
    /// Its child ended.
    Exited(ExitStatus),
}

/// How a pane's child ended, reported faithfully: a status and a signal of
/// the same number are not the same thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExitStatus {
    /// It exited, with this status.
    Exited(i32),
    /// It was killed by this signal.
    Signalled(i32),
}

/// Appends a removal reason: its tag, and the exit status when there is one.
fn put_reason(sink: &mut dyn Sink, reason: RemovalReason) {
    match reason {
        RemovalReason::Closed => sink.put(&[reason_tag::CLOSED]),
        RemovalReason::Exited(ExitStatus::Exited(status)) => {
            sink.put(&[reason_tag::EXITED, status_tag::EXITED]);
            sink.put(&status.to_le_bytes());
        }
        RemovalReason::Exited(ExitStatus::Signalled(signal)) => {
            sink.put(&[reason_tag::EXITED, status_tag::SIGNALLED]);
            sink.put(&signal.to_le_bytes());
        }
    }
}

/// The removal reason at the reader.
///
/// # Errors
///
/// The refusals [`decode_delta`] documents.
fn read_reason(reader: &mut Reader<'_>) -> Result<RemovalReason, MessageError> {
    match reader.byte()? {
        reason_tag::CLOSED => Ok(RemovalReason::Closed),
        reason_tag::EXITED => {
            // The tag is refused before its value is read, so a byte no
            // variant claims is named as that and not as bytes running out.
            let status: fn(i32) -> ExitStatus = match reader.byte()? {
                status_tag::EXITED => ExitStatus::Exited,
                status_tag::SIGNALLED => ExitStatus::Signalled,
                other => return Err(unknown(other)),
            };
            Ok(RemovalReason::Exited(status(i32::from_le_bytes(
                reader.array()?,
            ))))
        }
        other => Err(unknown(other)),
    }
}

/// Appends a delta: its discriminant and then its fields.
///
/// Neither half below knows every one of them — one match of that many arms
/// and their fields is longer than a function may be — so each says whether it
/// wrote what it was given, and this offers it to the other when it did not. A
/// variant moved between the two is therefore written by the half that knows
/// it rather than by neither, and `check_layouts` is exhaustive, so a new one
/// is a compile error there rather than a delta that encodes to nothing.
fn put_delta(sink: &mut dyn Sink, delta: &Delta) {
    if !put_arrangement(sink, delta) {
        let _written = put_pane_change(sink, delta);
    }
}

/// Refuses a delta carrying a layout nested past what a model holds. It is
/// exhaustive for the same reason [`put_delta`] is: a new variant carrying a
/// layout must not slip past the guard unnoticed.
///
/// # Errors
///
/// [`MessageError::LayoutTooDeep`], naming the bound.
fn check_layouts(delta: &Delta) -> Result<(), MessageError> {
    match delta {
        Delta::SessionAdded { session } => session
            .tabs
            .iter()
            .try_for_each(|tab| check_depth(&tab.layout)),
        Delta::TabAdded { tab, .. } => check_depth(&tab.layout),
        Delta::LayoutChanged { layout, .. } => check_depth(layout),
        Delta::SessionRenamed { .. }
        | Delta::SessionRemoved { .. }
        | Delta::TabRenamed { .. }
        | Delta::TabRemoved { .. }
        | Delta::TabsReordered { .. }
        | Delta::PaneAdded { .. }
        | Delta::PaneRemoved { .. }
        | Delta::PaneMoved { .. }
        | Delta::PaneTitle { .. }
        | Delta::PaneWorkingDirectory { .. }
        | Delta::PaneResized { .. }
        | Delta::SessionsReordered { .. } => Ok(()),
    }
}

/// Appends the deltas that name a session or a tab.
fn put_arrangement(sink: &mut dyn Sink, delta: &Delta) -> bool {
    match delta {
        Delta::SessionAdded { session } => {
            sink.put(&[delta_tag::SESSION_ADDED]);
            put_session(sink, session);
        }
        Delta::SessionsReordered { order } => {
            sink.put(&[delta_tag::SESSIONS_REORDERED]);
            put_count(sink, order.len());
            for session in order {
                sink.put(&session.0.to_le_bytes());
            }
        }
        Delta::SessionRenamed { session, name } => {
            sink.put(&[delta_tag::SESSION_RENAMED]);
            sink.put(&session.0.to_le_bytes());
            put_bytes(sink, name.as_bytes());
        }
        Delta::SessionRemoved { session } => {
            sink.put(&[delta_tag::SESSION_REMOVED]);
            sink.put(&session.0.to_le_bytes());
        }
        Delta::TabAdded {
            session,
            tab,
            index,
        } => {
            sink.put(&[delta_tag::TAB_ADDED]);
            sink.put(&session.0.to_le_bytes());
            put_tab(sink, tab);
            // An index is a position in a `Vec`, written the width a count is.
            put_count(sink, *index);
        }
        Delta::TabRenamed { tab, name } => {
            sink.put(&[delta_tag::TAB_RENAMED]);
            sink.put(&tab.0.to_le_bytes());
            put_bytes(sink, name.as_bytes());
        }
        Delta::TabRemoved { tab } => {
            sink.put(&[delta_tag::TAB_REMOVED]);
            sink.put(&tab.0.to_le_bytes());
        }
        Delta::TabsReordered { session, order } => {
            sink.put(&[delta_tag::TABS_REORDERED]);
            sink.put(&session.0.to_le_bytes());
            put_count(sink, order.len());
            for tab in order {
                sink.put(&tab.0.to_le_bytes());
            }
        }
        Delta::LayoutChanged { tab, layout } => {
            sink.put(&[delta_tag::LAYOUT_CHANGED]);
            sink.put(&tab.0.to_le_bytes());
            put_layout(sink, layout);
        }
        _other => return false,
    }
    true
}

/// Appends the deltas that name a pane.
fn put_pane_change(sink: &mut dyn Sink, delta: &Delta) -> bool {
    match delta {
        Delta::PaneAdded { tab, pane } => {
            sink.put(&[delta_tag::PANE_ADDED]);
            sink.put(&tab.0.to_le_bytes());
            put_pane(sink, pane);
        }
        Delta::PaneRemoved { pane, reason } => {
            sink.put(&[delta_tag::PANE_REMOVED]);
            sink.put(&pane.0.to_le_bytes());
            put_reason(sink, *reason);
        }
        Delta::PaneMoved { pane, to_tab } => {
            sink.put(&[delta_tag::PANE_MOVED]);
            sink.put(&pane.0.to_le_bytes());
            sink.put(&to_tab.0.to_le_bytes());
        }
        Delta::PaneTitle { pane, title } => {
            sink.put(&[delta_tag::PANE_TITLE]);
            sink.put(&pane.0.to_le_bytes());
            put_bytes(sink, title.as_bytes());
        }
        Delta::PaneWorkingDirectory { pane, path } => {
            sink.put(&[delta_tag::PANE_WORKING_DIRECTORY]);
            sink.put(&pane.0.to_le_bytes());
            put_bytes(sink, path.as_bytes());
        }
        Delta::PaneResized {
            pane,
            columns,
            rows,
        } => {
            sink.put(&[delta_tag::PANE_RESIZED]);
            sink.put(&pane.0.to_le_bytes());
            sink.put(&columns.to_le_bytes());
            sink.put(&rows.to_le_bytes());
        }
        _other => return false,
    }
    true
}

/// The `Delta` payload for a change.
///
/// # Errors
///
/// [`MessageError::LayoutTooDeep`] when a layout the delta carries nests past
/// [`crate::model::MAXIMUM_LAYOUT_DEPTH`], so nothing this encoder produces is
/// something [`decode_delta`] refuses, and [`MessageError::Oversize`] when the
/// encoding would not fit a frame, measured before anything is allocated.
pub fn encode_delta(delta: &Delta) -> Result<Vec<u8>, MessageError> {
    check_layouts(delta)?;
    encode(|sink| put_delta(sink, delta))
}

/// The change a `Delta` payload holds.
///
/// The bytes are not trusted: a count is read as far as the bytes go rather
/// than allocated for, and a layout is refused at
/// [`crate::model::MAXIMUM_LAYOUT_DEPTH`] on the way down.
///
/// # Errors
///
/// [`MessageError::UnknownDiscriminant`] for a delta, removal reason, exit
/// status, layout tag, split direction or presence byte no variant claims;
/// [`MessageError::Truncated`] when a field ends early;
/// [`MessageError::TrailingBytes`] when bytes follow the last field;
/// [`MessageError::Utf8`] when a name is not UTF-8; and
/// [`MessageError::LayoutTooDeep`] for a layout nested past what a model
/// holds. Every refusal names the delta whose field ran out.
pub fn decode_delta(bytes: &[u8]) -> Result<Delta, MessageError> {
    let mut reader = Reader::new(bytes)?;
    let delta = read_delta(&mut reader)?;
    reader.finish()?;
    Ok(delta)
}

/// The delta at the reader, whose discriminant it has already read.
///
/// # Errors
///
/// The refusals [`decode_delta`] documents.
fn read_delta(reader: &mut Reader<'_>) -> Result<Delta, MessageError> {
    match reader.discriminant {
        delta_tag::SESSION_ADDED => Ok(Delta::SessionAdded {
            session: read_session(reader)?,
        }),
        delta_tag::SESSION_RENAMED => Ok(Delta::SessionRenamed {
            session: SessionId(u64::from_le_bytes(reader.array()?)),
            name: reader.string()?,
        }),
        delta_tag::SESSION_REMOVED => Ok(Delta::SessionRemoved {
            session: SessionId(u64::from_le_bytes(reader.array()?)),
        }),
        delta_tag::TAB_ADDED => Ok(Delta::TabAdded {
            session: SessionId(u64::from_le_bytes(reader.array()?)),
            tab: read_tab(reader)?,
            index: reader.count()?,
        }),
        delta_tag::TAB_RENAMED => Ok(Delta::TabRenamed {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
            name: reader.string()?,
        }),
        delta_tag::TAB_REMOVED => Ok(Delta::TabRemoved {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
        }),
        delta_tag::TABS_REORDERED => read_reorder(reader),
        delta_tag::LAYOUT_CHANGED => Ok(Delta::LayoutChanged {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
            layout: read_layout(reader, ROOT_DEPTH)?,
        }),
        delta_tag::SESSIONS_REORDERED => read_session_order(reader),
        _other => read_pane_change(reader),
    }
}

/// The tab reorder at the reader, whose whole order follows its count.
///
/// # Errors
///
/// The refusals [`decode_delta`] documents.
fn read_reorder(reader: &mut Reader<'_>) -> Result<Delta, MessageError> {
    let session = SessionId(u64::from_le_bytes(reader.array()?));
    let count = reader.count()?;
    let mut order = Vec::new();
    for _index in 0..count {
        order.push(TabId(u64::from_le_bytes(reader.array()?)));
    }
    Ok(Delta::TabsReordered { session, order })
}

/// The session reorder at the reader, whose whole order follows its count.
///
/// # Errors
///
/// The refusals [`decode_delta`] documents.
fn read_session_order(reader: &mut Reader<'_>) -> Result<Delta, MessageError> {
    let count = reader.count()?;
    let mut order = Vec::new();
    for _index in 0..count {
        order.push(SessionId(u64::from_le_bytes(reader.array()?)));
    }
    Ok(Delta::SessionsReordered { order })
}

/// The pane delta at the reader, whose discriminant it has already read.
///
/// # Errors
///
/// The refusals [`decode_delta`] documents.
fn read_pane_change(reader: &mut Reader<'_>) -> Result<Delta, MessageError> {
    match reader.discriminant {
        delta_tag::PANE_ADDED => Ok(Delta::PaneAdded {
            tab: TabId(u64::from_le_bytes(reader.array()?)),
            pane: read_pane(reader)?,
        }),
        delta_tag::PANE_REMOVED => Ok(Delta::PaneRemoved {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            reason: read_reason(reader)?,
        }),
        delta_tag::PANE_MOVED => Ok(Delta::PaneMoved {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            to_tab: TabId(u64::from_le_bytes(reader.array()?)),
        }),
        delta_tag::PANE_TITLE => Ok(Delta::PaneTitle {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            title: reader.string()?,
        }),
        delta_tag::PANE_WORKING_DIRECTORY => Ok(Delta::PaneWorkingDirectory {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            path: reader.string()?,
        }),
        delta_tag::PANE_RESIZED => Ok(Delta::PaneResized {
            pane: PaneId(u64::from_le_bytes(reader.array()?)),
            columns: u16::from_le_bytes(reader.array()?),
            rows: u16::from_le_bytes(reader.array()?),
        }),
        other => Err(unknown(other)),
    }
}
