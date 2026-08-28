//! Applying a numbered delta to a host model: exactly the next generation,
//! every invariant checked before the first mutation.
//!
//! This is the one implementation both ends use — the server applies every
//! delta to its own model through it before anyone else sees the delta, and
//! every client applies the same delta to the snapshot it holds — so the
//! property the whole client design rests on, that a snapshot followed by
//! deltas converges on the next snapshot, is a property of one function.
//!
//! A delta numbered other than the model's generation plus one is refused with
//! [`ReconcileError::GenerationGap`], which is the client's cue to ask for a
//! snapshot rather than guess. Everything else it refuses is a delta that does
//! not fit the model it names: an identity the host does not hold, an identity
//! it already holds, an order that is not a permutation, or a value that is
//! not a well-formed part of a model. A refusal leaves the model exactly as it
//! was, which is why every check runs before the first mutation.
//!
//! What it does **not** refuse is a delta that leaves the model mid-change. A
//! pane appearing and the layout that places it are two deltas, because each
//! variant is the smallest thing that can happen, so between them a tab holds
//! a pane its layout does not place — and the same goes for a pane removed
//! before its layout, a tab removed before its session, and a pane moved
//! before either tab is rearranged. Refusing those would refuse the very
//! sequences the server emits.
//!
//! Every layout a delta carries is normalized as it is applied. A peer's tree
//! is not trusted to be canonical, and storing it uncanonical would make two
//! models that hold the same arrangement compare unequal.

use std::collections::HashSet;

use core::fmt::{self, Display, Formatter};

use crate::delta::Delta;
use crate::identity::{Generation, PaneId, SessionId, TabId};
use crate::model::{
    HostModel, LayoutNode, ModelError, Pane, SeenIds, Session, Tab, validate_layout,
    validate_session, validate_tab,
};

/// Why a delta could not be applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileError {
    /// The delta is not the model's next one. The client has missed a
    /// generation and asks for a snapshot rather than guessing what it held.
    GenerationGap {
        /// The generation the model would have accepted.
        expected: Generation,
        /// The generation the delta carried.
        received: Generation,
    },
    /// The delta names a session the host does not hold.
    UnknownSession {
        /// The session it names.
        session: SessionId,
    },
    /// The delta names a tab the host does not hold.
    UnknownTab {
        /// The tab it names.
        tab: TabId,
    },
    /// The delta names a pane the host does not hold.
    UnknownPane {
        /// The pane it names.
        pane: PaneId,
    },
    /// A tab was to be placed past the end of its session's tabs.
    IndexPastEnd {
        /// The session it was to join.
        session: SessionId,
        /// Where it was to go.
        index: usize,
        /// How many tabs the session holds.
        tabs: usize,
    },
    /// A reorder's order is not a permutation of the session's tabs, so
    /// applying it would lose a tab or invent one.
    NotAPermutation {
        /// The session whose tabs they are.
        session: SessionId,
    },
    /// The value the delta carries is not a well-formed part of a model, or
    /// mints an identity the host already holds.
    Invalid {
        /// Which invariant, and the identity that breaks it.
        error: ModelError,
    },
}

impl Display for ReconcileError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ReconcileError::GenerationGap { expected, received } => write!(
                formatter,
                "a delta numbered {} is not generation {}, the model's next",
                received.0, expected.0
            ),
            ReconcileError::UnknownSession { session } => {
                write!(formatter, "the host holds no session {}", session.0)
            }
            ReconcileError::UnknownTab { tab } => {
                write!(formatter, "the host holds no tab {}", tab.0)
            }
            ReconcileError::UnknownPane { pane } => {
                write!(formatter, "the host holds no pane {}", pane.0)
            }
            ReconcileError::IndexPastEnd {
                session,
                index,
                tabs,
            } => write!(
                formatter,
                "session {} holds {tabs} tabs, so a tab cannot go at {index}",
                session.0
            ),
            ReconcileError::NotAPermutation { session } => write!(
                formatter,
                "the order given is not a permutation of session {}'s tabs",
                session.0
            ),
            ReconcileError::Invalid { error } => write!(formatter, "{error}"),
        }
    }
}

impl core::error::Error for ReconcileError {}

/// Applies one numbered delta.
///
/// # Errors
///
/// [`ReconcileError::GenerationGap`] when `generation` is not the model's plus
/// one, and one of the others when the delta does not fit the model it names.
/// A refusal leaves the model exactly as it was.
pub fn apply(
    model: &mut HostModel,
    generation: Generation,
    delta: &Delta,
) -> Result<(), ReconcileError> {
    let expected = Generation(model.generation.0.saturating_add(1));
    if generation != expected {
        return Err(ReconcileError::GenerationGap {
            expected,
            received: generation,
        });
    }
    apply_change(model, delta)?;
    model.generation = generation;
    Ok(())
}

/// Applies the change a delta describes, leaving the generation alone.
///
/// Public because a client applies a command's effect before the server has
/// numbered it: an optimistic rename must show at once and must *not* advance
/// the generation, or the authoritative delta that follows would arrive as a
/// gap. What the client applies here is the same change the server will send,
/// through the same code, which is what makes the two agree.
///
/// # Errors
///
/// The refusals [`apply`] documents, save the generation gap.
pub fn apply_change(model: &mut HostModel, delta: &Delta) -> Result<(), ReconcileError> {
    match delta {
        Delta::SessionAdded { session } => add_session(model, session),
        Delta::SessionRenamed { session, name } => rename_session(model, *session, name),
        Delta::SessionRemoved { session } => remove_session(model, *session),
        Delta::TabAdded {
            session,
            tab,
            index,
        } => add_tab(model, *session, tab, *index),
        Delta::TabRenamed { tab, name } => rename_tab(model, *tab, name),
        Delta::TabRemoved { tab } => remove_tab(model, *tab),
        Delta::TabsReordered { session, order } => reorder_tabs(model, *session, order),
        Delta::PaneAdded { tab, pane } => add_pane(model, *tab, pane),
        Delta::PaneRemoved { pane, .. } => remove_pane(model, *pane),
        Delta::PaneMoved { pane, to_tab } => move_pane(model, *pane, *to_tab),
        Delta::LayoutChanged { tab, layout } => change_layout(model, *tab, layout),
        Delta::PaneTitle { pane, title } => set_title(model, *pane, title),
        Delta::PaneWorkingDirectory { pane, path } => set_directory(model, *pane, path),
        Delta::PaneResized {
            pane,
            columns,
            rows,
        } => resize_pane(model, *pane, *columns, *rows),
    }
}

/// The refusal a broken invariant becomes.
fn invalid(error: ModelError) -> ReconcileError {
    ReconcileError::Invalid { error }
}

/// Every tab the host holds, in order.
fn tabs(model: &HostModel) -> impl Iterator<Item = &Tab> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
}

/// Every tab the host holds, to change.
fn tabs_mut(model: &mut HostModel) -> impl Iterator<Item = &mut Tab> {
    model
        .sessions
        .iter_mut()
        .flat_map(|session| session.tabs.iter_mut())
}

/// The tab with this id, to change.
fn find_tab(model: &mut HostModel, tab: TabId) -> Option<&mut Tab> {
    tabs_mut(model).find(|held| held.id == tab)
}

/// The pane with this id, to change.
fn find_pane(model: &mut HostModel, pane: PaneId) -> Option<&mut Pane> {
    tabs_mut(model)
        .flat_map(|tab| tab.panes.iter_mut())
        .find(|held| held.id == pane)
}

/// The tab holding this pane.
fn tab_holding(model: &HostModel, pane: PaneId) -> Option<&Tab> {
    tabs(model).find(|tab| tab.panes.iter().any(|held| held.id == pane))
}

/// Whether the host already holds a session with this id.
fn holds_session(model: &HostModel, session: SessionId) -> bool {
    model.sessions.iter().any(|held| held.id == session)
}

/// Whether the host already holds a tab with this id.
fn holds_tab(model: &HostModel, tab: TabId) -> bool {
    tabs(model).any(|held| held.id == tab)
}

/// Whether the host already holds a pane with this id.
fn holds_pane(model: &HostModel, pane: PaneId) -> bool {
    tabs(model).any(|tab| tab.panes.iter().any(|held| held.id == pane))
}

/// The tab with its layout normalized, which is the form a model stores.
fn normalized_tab(tab: &Tab) -> Tab {
    Tab {
        id: tab.id,
        name: tab.name.clone(),
        panes: tab.panes.clone(),
        layout: tab.layout.clone().normalize(),
    }
}

/// The session with every tab's layout normalized.
fn normalized_session(session: &Session) -> Session {
    Session {
        id: session.id,
        name: session.name.clone(),
        tabs: session.tabs.iter().map(normalized_tab).collect(),
    }
}

/// Refuses a tab that mints an id the host already holds.
///
/// # Errors
///
/// [`ReconcileError::Invalid`] naming the identity minted twice.
fn fresh_tab_ids(model: &HostModel, tab: &Tab) -> Result<(), ReconcileError> {
    if holds_tab(model, tab.id) {
        return Err(invalid(ModelError::DuplicateTab { tab: tab.id }));
    }
    for pane in &tab.panes {
        if holds_pane(model, pane.id) {
            return Err(invalid(ModelError::DuplicatePane { pane: pane.id }));
        }
    }
    Ok(())
}

/// Adds a session, with its first tab and that tab's first pane.
///
/// # Errors
///
/// [`ReconcileError::Invalid`] when the session is not well-formed or mints an
/// identity the host already holds.
fn add_session(model: &mut HostModel, session: &Session) -> Result<(), ReconcileError> {
    let added = normalized_session(session);
    validate_session(&mut SeenIds::default(), &added).map_err(invalid)?;
    if holds_session(model, added.id) {
        return Err(invalid(ModelError::DuplicateSession { session: added.id }));
    }
    for tab in &added.tabs {
        fresh_tab_ids(model, tab)?;
    }
    model.sessions.push(added);
    Ok(())
}

/// Renames a session.
///
/// # Errors
///
/// [`ReconcileError::UnknownSession`] when the host holds no such session, and
/// [`ReconcileError::Invalid`] when the name is empty — the identity first, so
/// a refusal never names something the host does not hold.
fn rename_session(
    model: &mut HostModel,
    session: SessionId,
    name: &str,
) -> Result<(), ReconcileError> {
    let Some(held) = model.sessions.iter_mut().find(|held| held.id == session) else {
        return Err(ReconcileError::UnknownSession { session });
    };
    if name.is_empty() {
        return Err(invalid(ModelError::EmptySessionName { session }));
    }
    name.clone_into(&mut held.name);
    Ok(())
}

/// Removes a session and everything under it.
///
/// # Errors
///
/// [`ReconcileError::UnknownSession`] when the host holds no such session.
fn remove_session(model: &mut HostModel, session: SessionId) -> Result<(), ReconcileError> {
    if !holds_session(model, session) {
        return Err(ReconcileError::UnknownSession { session });
    }
    model.sessions.retain(|held| held.id != session);
    Ok(())
}

/// Adds a tab to a session at a place in its order.
///
/// # Errors
///
/// [`ReconcileError::Invalid`] when the tab is not well-formed or mints an
/// identity the host already holds, [`ReconcileError::UnknownSession`] when
/// the host holds no such session, and [`ReconcileError::IndexPastEnd`] when
/// the place is past the end.
fn add_tab(
    model: &mut HostModel,
    session: SessionId,
    tab: &Tab,
    index: usize,
) -> Result<(), ReconcileError> {
    let added = normalized_tab(tab);
    validate_tab(&mut SeenIds::default(), &added).map_err(invalid)?;
    fresh_tab_ids(model, &added)?;
    let Some(holder) = model.sessions.iter_mut().find(|held| held.id == session) else {
        return Err(ReconcileError::UnknownSession { session });
    };
    let tabs = holder.tabs.len();
    if index > tabs {
        return Err(ReconcileError::IndexPastEnd {
            session,
            index,
            tabs,
        });
    }
    // `insert` is in range: the line above is what says so.
    holder.tabs.insert(index, added);
    Ok(())
}

/// Renames a tab.
///
/// # Errors
///
/// [`ReconcileError::UnknownTab`] when the host holds no such tab, and
/// [`ReconcileError::Invalid`] when the name is empty.
fn rename_tab(model: &mut HostModel, tab: TabId, name: &str) -> Result<(), ReconcileError> {
    let Some(held) = find_tab(model, tab) else {
        return Err(ReconcileError::UnknownTab { tab });
    };
    if name.is_empty() {
        return Err(invalid(ModelError::EmptyTabName { tab }));
    }
    name.clone_into(&mut held.name);
    Ok(())
}

/// Removes a tab and every pane in it.
///
/// # Errors
///
/// [`ReconcileError::UnknownTab`] when the host holds no such tab.
fn remove_tab(model: &mut HostModel, tab: TabId) -> Result<(), ReconcileError> {
    let holder = model
        .sessions
        .iter_mut()
        .find(|held| held.tabs.iter().any(|found| found.id == tab));
    let Some(holder) = holder else {
        return Err(ReconcileError::UnknownTab { tab });
    };
    holder.tabs.retain(|found| found.id != tab);
    Ok(())
}

/// Whether an order names each of a session's tabs exactly once.
fn is_permutation(order: &[TabId], held: &[Tab]) -> bool {
    let wanted: HashSet<TabId> = order.iter().copied().collect();
    wanted.len() == order.len()
        && order.len() == held.len()
        && held.iter().all(|tab| wanted.contains(&tab.id))
}

/// Puts a session's tabs in the order given, which is the whole order.
///
/// # Errors
///
/// [`ReconcileError::UnknownSession`] when the host holds no such session, and
/// [`ReconcileError::NotAPermutation`] when the order would lose a tab or
/// invent one.
fn reorder_tabs(
    model: &mut HostModel,
    session: SessionId,
    order: &[TabId],
) -> Result<(), ReconcileError> {
    let Some(held) = model.sessions.iter_mut().find(|held| held.id == session) else {
        return Err(ReconcileError::UnknownSession { session });
    };
    if !is_permutation(order, &held.tabs) {
        return Err(ReconcileError::NotAPermutation { session });
    }
    let mut arranged = Vec::with_capacity(order.len());
    for wanted in order {
        if let Some(place) = held.tabs.iter().position(|tab| tab.id == *wanted) {
            arranged.push(held.tabs.remove(place));
        }
    }
    held.tabs = arranged;
    Ok(())
}

/// Adds a pane to a tab. The layout that places it is the change that follows.
///
/// # Errors
///
/// [`ReconcileError::Invalid`] when the pane mints an identity the host
/// already holds, and [`ReconcileError::UnknownTab`] when the host holds no
/// such tab.
fn add_pane(model: &mut HostModel, tab: TabId, pane: &Pane) -> Result<(), ReconcileError> {
    if holds_pane(model, pane.id) {
        return Err(invalid(ModelError::DuplicatePane { pane: pane.id }));
    }
    let Some(held) = find_tab(model, tab) else {
        return Err(ReconcileError::UnknownTab { tab });
    };
    held.panes.push(pane.clone());
    Ok(())
}

/// Removes a pane from the tab holding it.
///
/// # Errors
///
/// [`ReconcileError::UnknownPane`] when the host holds no such pane.
fn remove_pane(model: &mut HostModel, pane: PaneId) -> Result<(), ReconcileError> {
    let Some(holding) = tab_holding(model, pane).map(|tab| tab.id) else {
        return Err(ReconcileError::UnknownPane { pane });
    };
    if let Some(held) = find_tab(model, holding) {
        held.panes.retain(|found| found.id != pane);
    }
    Ok(())
}

/// Moves a pane to another tab. Both layouts are set by the changes that
/// follow.
///
/// # Errors
///
/// [`ReconcileError::UnknownPane`] when the host holds no such pane, and
/// [`ReconcileError::UnknownTab`] when it holds no such destination.
fn move_pane(model: &mut HostModel, pane: PaneId, to_tab: TabId) -> Result<(), ReconcileError> {
    let Some(source) = tab_holding(model, pane).map(|tab| tab.id) else {
        return Err(ReconcileError::UnknownPane { pane });
    };
    if !holds_tab(model, to_tab) {
        return Err(ReconcileError::UnknownTab { tab: to_tab });
    }
    if source == to_tab {
        return Ok(());
    }
    let Some(moved) = find_pane(model, pane).map(|held| held.clone()) else {
        return Err(ReconcileError::UnknownPane { pane });
    };
    // The copy lands before the original goes, so a lookup that somehow found
    // nothing would leave the pane twice over rather than not at all.
    if let Some(destination) = find_tab(model, to_tab) {
        destination.panes.push(moved);
    }
    if let Some(held) = find_tab(model, source) {
        held.panes.retain(|found| found.id != pane);
    }
    Ok(())
}

/// Rearranges a tab, normalizing the layout it is given.
///
/// # Errors
///
/// [`ReconcileError::UnknownTab`] when the host holds no such tab, and
/// [`ReconcileError::Invalid`] when the layout does not place exactly the
/// tab's panes, each once, weighted and no deeper than a model holds.
fn change_layout(
    model: &mut HostModel,
    tab: TabId,
    layout: &LayoutNode,
) -> Result<(), ReconcileError> {
    let normalized = layout.clone().normalize();
    let Some(held) = find_tab(model, tab) else {
        return Err(ReconcileError::UnknownTab { tab });
    };
    validate_layout(tab, &held.panes, &normalized).map_err(invalid)?;
    held.layout = normalized;
    Ok(())
}

/// Sets a pane's title.
///
/// # Errors
///
/// [`ReconcileError::UnknownPane`] when the host holds no such pane.
fn set_title(model: &mut HostModel, pane: PaneId, title: &str) -> Result<(), ReconcileError> {
    let Some(held) = find_pane(model, pane) else {
        return Err(ReconcileError::UnknownPane { pane });
    };
    title.clone_into(&mut held.title);
    Ok(())
}

/// Sets where a pane's shell said it is.
///
/// # Errors
///
/// [`ReconcileError::UnknownPane`] when the host holds no such pane.
fn set_directory(model: &mut HostModel, pane: PaneId, path: &str) -> Result<(), ReconcileError> {
    let Some(held) = find_pane(model, pane) else {
        return Err(ReconcileError::UnknownPane { pane });
    };
    held.working_directory = Some(path.to_owned());
    Ok(())
}

/// Sets a pane's size.
///
/// # Errors
///
/// [`ReconcileError::UnknownPane`] when the host holds no such pane.
fn resize_pane(
    model: &mut HostModel,
    pane: PaneId,
    columns: u16,
    rows: u16,
) -> Result<(), ReconcileError> {
    let Some(held) = find_pane(model, pane) else {
        return Err(ReconcileError::UnknownPane { pane });
    };
    held.columns = columns;
    held.rows = rows;
    Ok(())
}
