//! The registry's operations and their delta order, the ingestion of pane
//! marks, sizes and exits, and the validation after every operation.
//!
//! This is the authoritative host model. Every change to it is a numbered
//! delta emitted by the operation that caused it, and every delta is applied
//! to the registry's own model **through the protocol's reconciler** before
//! anyone else sees it. That is what makes convergence something the registry
//! can be held to rather than something it hopes for: the model a client
//! rebuilds from the deltas and the model the registry holds are the same
//! model because they are the same application of the same function.
//!
//! Identity is minted once from counters that never reuse a value, and
//! position is derived: closing a tab leaves every surviving tab's id alone.
//!
//! A pane's marks, size and exit reach the model by [`Registry::ingest`],
//! which takes what the panes have reported and turns it into deltas. It
//! pulls rather than subscribes: a registry lives behind a lock, and a task
//! that took that lock on every mark would need a handle to the lock the
//! registry cannot hold until the lock exists. Whoever owns the registry calls
//! it when a pane signals.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use iznik_protocol::command::Placement;
use iznik_protocol::delta::{Delta, ExitStatus as EndedAs, RemovalReason};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::MarkKind;
use iznik_protocol::model;
use iznik_protocol::model::{HostModel, LayoutNode, ModelError, Session, Tab, Weighted};
use iznik_protocol::reconcile::{ReconcileError, apply};
use tokio::sync::{broadcast, watch};

use crate::history::ring::PaneHistory;
use crate::history::{DEFAULT_PANE_HISTORY_BYTES, HistoryBudget};
use crate::pane::{Pane, PaneError, PaneState};
use crate::pty::spawn::{ExitStatus, Program, Signal, SpawnOptions};
use crate::terminal::marks::MarkEvent;
use crate::terminal::mirror::MirrorThread;

/// How many deltas a client may fall behind before it must be told the whole
/// model instead. A receiver that lags and sees `Lagged` sends its client a
/// fresh `Snapshot`, which is what the client's reconciler would have asked
/// for anyway, so this is a memory bound and not a correctness one.
pub const DELTA_BROADCAST_CAPACITY: usize = 1024;

/// The weight each side of a new split gets: equal, because the server does
/// not know how large a cell is and the client that does can say otherwise.
const EVEN_WEIGHT: u32 = 1;

/// A value and the generation of the model it produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Numbered<Value> {
    /// The generation the change produces.
    pub generation: Generation,
    /// The change.
    pub value: Value,
}

/// What every pane a registry spawns has in common.
#[derive(Clone, Debug)]
pub struct RegistryDefaults {
    /// What to run in a pane. The daemon passes [`Program::LoginShell`],
    /// which is the product rule; a test passes `sh`, which is how a thousand
    /// operations finish in seconds instead of drawing a thousand prompts.
    pub program: Program,
    /// The terminfo a ghostty `TERM` needs, when the bootstrap installed it.
    pub terminfo_directory: Option<PathBuf>,
}

/// Why an operation could not be carried out.
#[derive(Debug)]
pub enum RegistryError {
    /// The host holds no such session.
    UnknownSession {
        /// The session named.
        session: SessionId,
    },
    /// The host holds no such tab.
    UnknownTab {
        /// The tab named.
        tab: TabId,
    },
    /// The host holds no such pane.
    UnknownPane {
        /// The pane named.
        pane: PaneId,
    },
    /// A name was empty, which no session or tab may carry.
    EmptyName,
    /// An order was not a permutation of the session's tabs.
    NotAPermutation {
        /// The session whose tabs they are.
        session: SessionId,
    },
    /// A layout does not place exactly the tab's panes, each once.
    InvalidLayout {
        /// The tab it was for.
        tab: TabId,
        /// What is wrong with it.
        error: ModelError,
    },
    /// A pseudoterminal or its child could not be started.
    Spawn(PaneError),
}

impl core::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RegistryError::UnknownSession { session } => {
                write!(formatter, "the host holds no session {}", session.0)
            }
            RegistryError::UnknownTab { tab } => {
                write!(formatter, "the host holds no tab {}", tab.0)
            }
            RegistryError::UnknownPane { pane } => {
                write!(formatter, "the host holds no pane {}", pane.0)
            }
            RegistryError::EmptyName => write!(formatter, "a name may not be empty"),
            RegistryError::NotAPermutation { session } => write!(
                formatter,
                "the order given is not a permutation of session {}'s tabs",
                session.0
            ),
            RegistryError::InvalidLayout { tab, error } => {
                write!(formatter, "the layout for tab {}: {error}", tab.0)
            }
            RegistryError::Spawn(error) => write!(formatter, "{error}"),
        }
    }
}

impl core::error::Error for RegistryError {}

impl From<PaneError> for RegistryError {
    fn from(error: PaneError) -> RegistryError {
        RegistryError::Spawn(error)
    }
}

/// What a pane has to report, and what the model last recorded of it.
#[derive(Debug)]
struct Watching {
    /// Its shell-integration marks.
    marks: broadcast::Receiver<MarkEvent>,
    /// Its size and history bounds.
    state: watch::Receiver<PaneState>,
    /// The size the model holds, so only a change is a delta.
    size: (u16, u16),
    /// Whether its exit has already become deltas.
    ended: bool,
}

/// The authoritative host model, the panes behind it, and the deltas every
/// change to it emits.
#[derive(Debug)]
pub struct Registry {
    /// The model itself.
    model: HostModel,
    /// The server-side pane behind every pane in the model.
    panes: BTreeMap<PaneId, Arc<Pane>>,
    /// What each pane has to report.
    watching: BTreeMap<PaneId, Watching>,
    /// The next session id to mint; never reused.
    next_session: u64,
    /// The next tab id to mint; never reused.
    next_tab: u64,
    /// The next pane id to mint; never reused.
    next_pane: u64,
    /// The history every pane's ring is taken out of.
    budget: Arc<Mutex<HistoryBudget>>,
    /// The thread every pane's mirror lives on.
    mirrors: MirrorThread,
    /// What every pane it spawns has in common.
    defaults: RegistryDefaults,
    /// Where every delta goes.
    deltas: broadcast::Sender<Numbered<Delta>>,
}

impl Registry {
    /// A registry holding nothing, spawning panes on `mirrors` out of
    /// `budget`.
    #[must_use]
    pub fn new(
        defaults: RegistryDefaults,
        budget: Arc<Mutex<HistoryBudget>>,
        mirrors: MirrorThread,
    ) -> Registry {
        let (deltas, _receiver) = broadcast::channel(DELTA_BROADCAST_CAPACITY);
        Registry {
            model: HostModel {
                generation: Generation(0),
                sessions: Vec::new(),
            },
            panes: BTreeMap::new(),
            watching: BTreeMap::new(),
            next_session: 1,
            next_tab: 1,
            next_pane: 1,
            budget,
            mirrors,
            defaults,
            deltas,
        }
    }

    /// The whole model as it stands.
    #[must_use]
    pub fn snapshot(&self) -> HostModel {
        self.model.clone()
    }

    /// The generation the model is at.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.model.generation
    }

    /// Every change from now on. A receiver that falls more than
    /// [`DELTA_BROADCAST_CAPACITY`] behind sees `Lagged`, and its client is
    /// sent a fresh snapshot instead.
    #[must_use]
    pub fn deltas(&self) -> broadcast::Receiver<Numbered<Delta>> {
        self.deltas.subscribe()
    }

    /// The pane behind a pane in the model.
    #[must_use]
    pub fn pane(&self, pane: PaneId) -> Option<&Arc<Pane>> {
        self.panes.get(&pane)
    }

    /// Emits one delta: applies it to the registry's own model through the
    /// protocol's reconciler, and only then broadcasts it.
    ///
    /// A delta the reconciler refuses is a bug in an operation above, and it
    /// is neither applied nor sent, so the model and what a client rebuilds
    /// from the deltas cannot come apart. It is logged, because a change a
    /// person asked for silently not happening is the thing worth seeing.
    fn emit(&mut self, delta: Delta) {
        let generation = Generation(self.model.generation.0.saturating_add(1));
        match apply(&mut self.model, generation, &delta) {
            Ok(()) => {
                let _receivers = self.deltas.send(Numbered {
                    generation,
                    value: delta,
                });
            }
            Err(error) => Registry::report(&error, &delta),
        }
    }

    /// Says that a delta the registry built was refused by the reconciler.
    fn report(error: &ReconcileError, delta: &Delta) {
        tracing::error!(%error, ?delta, "the registry built a delta the reconciler refused");
    }

    /// Confirms the model still holds together, which it must after every
    /// operation. It is a check and not an assertion: a model glitch degrades
    /// a client, and killing the server over it would cost everyone their
    /// terminals. The tests are what fail on it.
    fn settled(&self) {
        if cfg!(debug_assertions)
            && let Err(error) = self.model.validate()
        {
            tracing::error!(%error, "the registry's model does not hold together");
        }
    }

    /// The next session id, minted once.
    fn mint_session(&mut self) -> SessionId {
        let id = SessionId(self.next_session);
        self.next_session = self.next_session.saturating_add(1);
        id
    }

    /// The next tab id, minted once.
    fn mint_tab(&mut self) -> TabId {
        let id = TabId(self.next_tab);
        self.next_tab = self.next_tab.saturating_add(1);
        id
    }

    /// Spawns a pane, gives it the history the budget allows, and starts
    /// watching what it has to report.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Spawn`] when the pseudoterminal or its child cannot be
    /// started; nothing is minted and nothing is watched.
    async fn spawn_pane(
        &mut self,
        columns: u16,
        rows: u16,
        working_directory: Option<PathBuf>,
    ) -> Result<(PaneId, model::Pane), RegistryError> {
        let options = SpawnOptions {
            program: self.defaults.program.clone(),
            columns,
            rows,
            working_directory,
            terminfo_directory: self.defaults.terminfo_directory.clone(),
        };
        let id = PaneId(self.next_pane);
        let capacity = self.admit(id);
        let pane = Pane::spawn(&options, capacity, &self.mirrors).await?;
        // Minted only once the pane exists, so a failed spawn does not consume
        // an id and leave a hole a person could notice.
        self.next_pane = self.next_pane.saturating_add(1);
        self.watching.insert(
            id,
            Watching {
                marks: pane.marks(),
                state: pane.state_updates(),
                size: (columns, rows),
                ended: false,
            },
        );
        self.panes.insert(id, Arc::new(pane));
        self.apply_budget();
        Ok((
            id,
            model::Pane {
                id,
                title: String::new(),
                working_directory: None,
                columns,
                rows,
            },
        ))
    }

    /// Takes a pane's history out of the budget and says how much it may hold.
    fn admit(&self, pane: PaneId) -> usize {
        let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        budget.insert(pane, DEFAULT_PANE_HISTORY_BYTES);
        budget
            .history(pane)
            .map_or(DEFAULT_PANE_HISTORY_BYTES, PaneHistory::capacity)
    }

    /// Tells every pane how much history the budget now allows it, which is
    /// how a new pane takes memory back from the least recently focused one.
    fn apply_budget(&self) {
        let budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        for (id, pane) in &self.panes {
            if let Some(history) = budget.history(*id) {
                pane.set_history_capacity(history.capacity());
            }
        }
    }

    /// Says a pane was looked at, so the panes that were not shrink first.
    pub fn touch(&self, pane: PaneId) {
        let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        budget.touch(pane);
    }
}

/// What a session's first tab is called, since nothing named it.
const FIRST_TAB_NAME: &str = "shell";

/// The layout with the target's leaf replaced by a split of the target and the
/// new pane, weights equal, in the direction given, the new pane before or
/// after it — normalized, which is what flattens the split into a parent that
/// already divides that way.
fn split_at(layout: LayoutNode, placement: Placement, added: PaneId) -> LayoutNode {
    let target = Weighted {
        node: LayoutNode::Leaf(placement.target),
        weight: EVEN_WEIGHT,
    };
    let fresh = Weighted {
        node: LayoutNode::Leaf(added),
        weight: EVEN_WEIGHT,
    };
    let children = if placement.before {
        vec![fresh, target]
    } else {
        vec![target, fresh]
    };
    let mut arranged = layout;
    let _found = arranged.replace_leaf(
        placement.target,
        LayoutNode::Split {
            direction: placement.direction,
            children,
        },
    );
    arranged
}

impl Registry {
    /// Every tab the host holds.
    fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.model
            .sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
    }

    /// The tab with this id.
    fn tab(&self, tab: TabId) -> Option<&Tab> {
        self.tabs().find(|held| held.id == tab)
    }

    /// The tab holding this pane.
    fn tab_of_pane(&self, pane: PaneId) -> Option<TabId> {
        self.tabs()
            .find(|tab| tab.panes.iter().any(|held| held.id == pane))
            .map(|tab| tab.id)
    }

    /// The session holding this tab.
    fn session_of_tab(&self, tab: TabId) -> Option<SessionId> {
        self.model
            .sessions
            .iter()
            .find(|session| session.tabs.iter().any(|held| held.id == tab))
            .map(|session| session.id)
    }

    /// Whether the reconciler would take this delta, without applying it: the
    /// registry asks before it emits, so a refusal is an answer to whoever
    /// asked rather than a delta that goes nowhere.
    ///
    /// # Errors
    ///
    /// Whatever the reconciler would have refused it with.
    fn would_accept(&self, delta: &Delta) -> Result<(), ReconcileError> {
        let mut trial = self.model.clone();
        let generation = Generation(trial.generation.0.saturating_add(1));
        apply(&mut trial, generation, delta)
    }

    /// Makes a session, its first tab and that tab's first pane.
    ///
    /// # Errors
    ///
    /// [`RegistryError::EmptyName`] for an empty name, and
    /// [`RegistryError::Spawn`] when the pane cannot be started — in which
    /// case nothing is minted and nothing is emitted.
    pub async fn create_session(
        &mut self,
        name: String,
        columns: u16,
        rows: u16,
        working_directory: Option<PathBuf>,
    ) -> Result<SessionId, RegistryError> {
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        let (pane_id, pane) = self.spawn_pane(columns, rows, working_directory).await?;
        let tab = Tab {
            id: self.mint_tab(),
            name: FIRST_TAB_NAME.to_owned(),
            panes: vec![pane],
            layout: LayoutNode::Leaf(pane_id),
        };
        let session = Session {
            id: self.mint_session(),
            name,
            tabs: vec![tab],
        };
        let id = session.id;
        self.emit(Delta::SessionAdded { session });
        self.settled();
        Ok(id)
    }

    /// Makes a tab at the end of a session, with its first pane.
    ///
    /// # Errors
    ///
    /// [`RegistryError::EmptyName`], [`RegistryError::UnknownSession`], and
    /// [`RegistryError::Spawn`] when the pane cannot be started.
    pub async fn create_tab(
        &mut self,
        session: SessionId,
        name: String,
        columns: u16,
        rows: u16,
        working_directory: Option<PathBuf>,
    ) -> Result<TabId, RegistryError> {
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        let Some(index) = self
            .model
            .sessions
            .iter()
            .find(|held| held.id == session)
            .map(|held| held.tabs.len())
        else {
            return Err(RegistryError::UnknownSession { session });
        };
        let (pane_id, pane) = self.spawn_pane(columns, rows, working_directory).await?;
        let tab = Tab {
            id: self.mint_tab(),
            name,
            panes: vec![pane],
            layout: LayoutNode::Leaf(pane_id),
        };
        let id = tab.id;
        self.emit(Delta::TabAdded {
            session,
            tab,
            index,
        });
        self.settled();
        Ok(id)
    }

    /// Makes a pane beside one that is already in the tab.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownTab`], [`RegistryError::UnknownPane`] when the
    /// tab does not hold the pane the placement names,
    /// [`RegistryError::InvalidLayout`] when the arrangement would nest past
    /// what a model holds, and [`RegistryError::Spawn`].
    pub async fn create_pane(
        &mut self,
        tab: TabId,
        placement: Placement,
        columns: u16,
        rows: u16,
        working_directory: Option<PathBuf>,
    ) -> Result<PaneId, RegistryError> {
        let Some(held) = self.tab(tab) else {
            return Err(RegistryError::UnknownTab { tab });
        };
        if !held.panes.iter().any(|pane| pane.id == placement.target) {
            return Err(RegistryError::UnknownPane {
                pane: placement.target,
            });
        }
        // The id `spawn_pane` will mint, so the arrangement can be judged
        // before a process is started for it.
        let arranged = split_at(held.layout.clone(), placement, PaneId(self.next_pane));
        let depth = arranged.depth();
        if depth > model::MAXIMUM_LAYOUT_DEPTH {
            return Err(RegistryError::InvalidLayout {
                tab,
                error: ModelError::LayoutTooDeep { tab, depth },
            });
        }
        let (_id, pane) = self.spawn_pane(columns, rows, working_directory).await?;
        let id = pane.id;
        self.emit(Delta::PaneAdded { tab, pane });
        self.emit(Delta::LayoutChanged {
            tab,
            layout: arranged,
        });
        self.settled();
        Ok(id)
    }
}

impl Registry {
    /// Closes a pane and takes it out of the model, with the tab and the
    /// session behind it when it was the last of each.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownPane`].
    pub fn close_pane(&mut self, pane: PaneId) -> Result<(), RegistryError> {
        if !self.panes.contains_key(&pane) {
            return Err(RegistryError::UnknownPane { pane });
        }
        self.remove_pane(pane, RemovalReason::Closed);
        self.settled();
        Ok(())
    }

    /// Takes a pane out of the model and emits the cascade its going causes:
    /// the pane, then either the layout that no longer places it or the tab it
    /// emptied, and the session behind that.
    fn remove_pane(&mut self, pane: PaneId, reason: RemovalReason) {
        let Some(tab) = self.tab_of_pane(pane) else {
            return;
        };
        let layout = self.tab(tab).map(|held| held.layout.clone());
        self.emit(Delta::PaneRemoved { pane, reason });
        self.retire(pane);
        match layout.and_then(|held| held.remove_leaf(pane)) {
            Some(arranged) => self.emit(Delta::LayoutChanged {
                tab,
                layout: arranged,
            }),
            None => self.remove_tab(tab),
        }
    }

    /// Ends a pane's child and forgets it, without saying anything: the delta
    /// that removed it has already gone, or the tab's has.
    fn retire(&mut self, pane: PaneId) {
        if let Some(held) = self.panes.remove(&pane)
            && let Err(error) = held.close()
        {
            tracing::warn!(%error, pane = pane.0, "a pane did not close cleanly");
        }
        let _watched = self.watching.remove(&pane);
    }

    /// Takes a tab out of the model with every pane in it, and the session
    /// behind it when it was the last tab.
    fn remove_tab(&mut self, tab: TabId) {
        let session = self.session_of_tab(tab);
        let panes: Vec<PaneId> = self
            .tab(tab)
            .map(|held| held.panes.iter().map(|pane| pane.id).collect())
            .unwrap_or_default();
        self.emit(Delta::TabRemoved { tab });
        for pane in panes {
            self.retire(pane);
        }
        if let Some(session) = session
            && self
                .model
                .sessions
                .iter()
                .find(|held| held.id == session)
                .is_some_and(|held| held.tabs.is_empty())
        {
            self.emit(Delta::SessionRemoved { session });
        }
    }

    /// Closes a tab and every pane in it.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownTab`].
    pub fn close_tab(&mut self, tab: TabId) -> Result<(), RegistryError> {
        if self.tab(tab).is_none() {
            return Err(RegistryError::UnknownTab { tab });
        }
        self.remove_tab(tab);
        self.settled();
        Ok(())
    }

    /// Closes a session and everything under it.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownSession`].
    pub fn close_session(&mut self, session: SessionId) -> Result<(), RegistryError> {
        let Some(held) = self.model.sessions.iter().find(|found| found.id == session) else {
            return Err(RegistryError::UnknownSession { session });
        };
        let panes: Vec<PaneId> = held
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.iter().map(|pane| pane.id))
            .collect();
        self.emit(Delta::SessionRemoved { session });
        for pane in panes {
            self.retire(pane);
        }
        self.settled();
        Ok(())
    }

    /// Renames a session.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownSession`] and [`RegistryError::EmptyName`].
    pub fn rename_session(
        &mut self,
        session: SessionId,
        name: String,
    ) -> Result<(), RegistryError> {
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        if !self.model.sessions.iter().any(|held| held.id == session) {
            return Err(RegistryError::UnknownSession { session });
        }
        self.emit(Delta::SessionRenamed { session, name });
        self.settled();
        Ok(())
    }

    /// Renames a tab.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownTab`] and [`RegistryError::EmptyName`].
    pub fn rename_tab(&mut self, tab: TabId, name: String) -> Result<(), RegistryError> {
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        if self.tab(tab).is_none() {
            return Err(RegistryError::UnknownTab { tab });
        }
        self.emit(Delta::TabRenamed { tab, name });
        self.settled();
        Ok(())
    }

    /// Puts a session's tabs in another order, which must name each of them
    /// once.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownSession`] and
    /// [`RegistryError::NotAPermutation`].
    pub fn reorder_tabs(
        &mut self,
        session: SessionId,
        order: Vec<TabId>,
    ) -> Result<(), RegistryError> {
        if !self.model.sessions.iter().any(|held| held.id == session) {
            return Err(RegistryError::UnknownSession { session });
        }
        let delta = Delta::TabsReordered { session, order };
        if self.would_accept(&delta).is_err() {
            return Err(RegistryError::NotAPermutation { session });
        }
        self.emit(delta);
        self.settled();
        Ok(())
    }

    /// Arranges a tab another way. The layout is normalized as it is applied,
    /// and must place exactly the tab's panes, each once.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownTab`] and [`RegistryError::InvalidLayout`],
    /// naming what is wrong with it.
    pub fn set_layout(&mut self, tab: TabId, layout: LayoutNode) -> Result<(), RegistryError> {
        if self.tab(tab).is_none() {
            return Err(RegistryError::UnknownTab { tab });
        }
        let delta = Delta::LayoutChanged { tab, layout };
        match self.would_accept(&delta) {
            Ok(()) => {}
            Err(ReconcileError::Invalid { error }) => {
                return Err(RegistryError::InvalidLayout { tab, error });
            }
            Err(_other) => return Err(RegistryError::UnknownTab { tab }),
        }
        self.emit(delta);
        self.settled();
        Ok(())
    }

    /// Moves a pane into another tab, beside one that is already there.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownPane`], [`RegistryError::UnknownTab`], and
    /// [`RegistryError::InvalidLayout`] when the destination would nest past
    /// what a model holds.
    pub fn move_pane(
        &mut self,
        pane: PaneId,
        to_tab: TabId,
        placement: Placement,
    ) -> Result<(), RegistryError> {
        let Some(source) = self.tab_of_pane(pane) else {
            return Err(RegistryError::UnknownPane { pane });
        };
        let Some(destination) = self.tab(to_tab) else {
            return Err(RegistryError::UnknownTab { tab: to_tab });
        };
        if !destination
            .panes
            .iter()
            .any(|held| held.id == placement.target)
        {
            return Err(RegistryError::UnknownPane {
                pane: placement.target,
            });
        }
        if source == to_tab {
            return Ok(());
        }
        let arranged = split_at(destination.layout.clone(), placement, pane);
        let depth = arranged.depth();
        if depth > model::MAXIMUM_LAYOUT_DEPTH {
            return Err(RegistryError::InvalidLayout {
                tab: to_tab,
                error: ModelError::LayoutTooDeep { tab: to_tab, depth },
            });
        }
        let vacated = self
            .tab(source)
            .map(|held| held.layout.clone())
            .and_then(|held| held.remove_leaf(pane));
        self.emit(Delta::PaneMoved { pane, to_tab });
        match vacated {
            Some(layout) => self.emit(Delta::LayoutChanged {
                tab: source,
                layout,
            }),
            None => self.remove_tab(source),
        }
        self.emit(Delta::LayoutChanged {
            tab: to_tab,
            layout: arranged,
        });
        self.settled();
        Ok(())
    }
}

/// `SIGHUP`'s number, which is what a client is told when a pane's child was
/// hung up rather than exiting on its own.
const HANGUP_SIGNAL: i32 = 1;

/// `SIGKILL`'s number.
const KILL_SIGNAL: i32 = 9;

/// `SIGTERM`'s number.
const TERMINATE_SIGNAL: i32 = 15;

impl Registry {
    /// Turns everything the panes have reported since the last call into
    /// deltas: titles and working directories from their marks, sizes from
    /// their state, and the removal cascade from an exit.
    ///
    /// It takes what is there and does not wait, so whoever owns the registry
    /// calls it when a pane signals.
    pub fn ingest(&mut self) {
        let panes: Vec<PaneId> = self.watching.keys().copied().collect();
        for pane in panes {
            self.ingest_marks(pane);
            self.ingest_state(pane);
        }
        self.settled();
    }

    /// The deltas a pane's marks have become. A mark the model does not hold —
    /// a prompt, a command's start or end, an alternate-screen switch — is a
    /// client's business and not the model's; the multiplexer forwards it.
    fn ingest_marks(&mut self, pane: PaneId) {
        let mut deltas = Vec::new();
        if let Some(watching) = self.watching.get_mut(&pane) {
            loop {
                match watching.marks.try_recv() {
                    Ok(event) => match event.kind {
                        MarkKind::Title { text } => {
                            deltas.push(Delta::PaneTitle { pane, title: text });
                        }
                        MarkKind::WorkingDirectory { path } => {
                            deltas.push(Delta::PaneWorkingDirectory { pane, path });
                        }
                        MarkKind::PromptStart
                        | MarkKind::CommandStart
                        | MarkKind::CommandExecuted
                        | MarkKind::CommandFinished { .. }
                        | MarkKind::AlternateScreen { .. } => {}
                    },
                    // A client that fell behind on marks still gets the model
                    // right; the ring is the durable record of the bytes.
                    Err(broadcast::error::TryRecvError::Lagged(_missed)) => {}
                    Err(_gone) => break,
                }
            }
        }
        for delta in deltas {
            self.emit(delta);
        }
    }

    /// The deltas a pane's size and end have become.
    fn ingest_state(&mut self, pane: PaneId) {
        let Some(watching) = self.watching.get_mut(&pane) else {
            return;
        };
        // A closed sender still holds the last state it published, and that is
        // the one that says the child has gone.
        if !watching.state.has_changed().unwrap_or(true) {
            return;
        }
        let state = *watching.state.borrow_and_update();
        let resized = (state.columns, state.rows) != watching.size;
        watching.size = (state.columns, state.rows);
        let ending = state.exited && !watching.ended;
        if resized {
            self.emit(Delta::PaneResized {
                pane,
                columns: state.columns,
                rows: state.rows,
            });
        }
        if !ending {
            return;
        }
        // The reaper records the status a moment after the stream closes. The
        // pane's going is not reported until it can be reported truthfully:
        // there is no code that stands in for a signal.
        let Some(status) = self
            .panes
            .get(&pane)
            .and_then(|held| held.exit_status_now())
        else {
            return;
        };
        if let Some(recorded) = self.watching.get_mut(&pane) {
            recorded.ended = true;
        }
        self.remove_pane(pane, RemovalReason::Exited(ended_as(status)));
    }
}

/// How a child's end is told to a client: a code it chose, or the number of
/// the signal that ended it.
fn ended_as(status: ExitStatus) -> EndedAs {
    match status {
        ExitStatus::Exited(code) => EndedAs::Exited(code),
        ExitStatus::Signalled(Signal::Hangup) => EndedAs::Signalled(HANGUP_SIGNAL),
        ExitStatus::Signalled(Signal::Kill) => EndedAs::Signalled(KILL_SIGNAL),
        ExitStatus::Signalled(Signal::Terminate) => EndedAs::Signalled(TERMINATE_SIGNAL),
    }
}
