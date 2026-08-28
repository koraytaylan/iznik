//! The seeded generator of valid models, delta sequences and registry
//! operations that three crates' tests share.
//!
//! One implementation of what a valid sequence is. Copied into a second test
//! file it would be two, and the second would drift.
//!
//! The generator keeps a model and changes it **directly** — pushing to a
//! `Vec`, setting a name — while recording the deltas that describe the same
//! change. That is what makes the convergence property worth testing: the
//! deltas are checked against a model nobody built by applying them.
//!
//! A change is one or more deltas, because each delta is the smallest thing
//! that can happen: a pane appearing and the layout that places it are two.
//! The model is whole after a change, not between the deltas of one, so a test
//! validates where a change ends.
//!
//! Randomness is a hand-written xorshift over a `u64` seed, so the protocol
//! crate's tests add no dependency and a failure is reproduced by its seed.

use iznik_protocol::delta::{Delta, ExitStatus, RemovalReason};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, SplitDirection, Tab, Weighted};

/// The most sessions a generated model starts with.
const STARTING_SESSIONS: usize = 3;

/// The most tabs a generated session starts with.
const STARTING_TABS: usize = 3;

/// The most panes a generated tab starts with.
const STARTING_PANES: usize = 4;

/// The widths a generated pane is given, from this upward.
const SMALLEST_COLUMNS: u16 = 20;

/// The heights a generated pane is given, from this upward.
const SMALLEST_ROWS: u16 = 5;

/// How much larger than the smallest a generated pane may be.
const SIZE_SPREAD: u64 = 200;

/// The largest weight a generated split hands out; small, so that a weight
/// scaled by a flattening stays legible in a failure.
const WEIGHT_SPREAD: u64 = 4;

/// The three shifts of the xorshift, which are the constants that make it a
/// full-period generator over sixty-four bits.
const FIRST_SHIFT: u32 = 13;

/// The second shift, to the right.
const SECOND_SHIFT: u32 = 7;

/// The third shift, to the left again.
const THIRD_SHIFT: u32 = 17;

/// The state a seed of zero starts from. Zero is a fixed point of the
/// xorshift — it maps to itself for ever — so the one seed that would draw
/// nothing but zeros is given a state that draws.
const ZERO_SEED_STATE: u64 = 0x9E37_79B9_7F4A_7C15;

/// The two ways a choice with two sides can go.
const EITHER: u64 = 2;

/// The fewest panes a tab must hold for one of them to be taken away, moved or
/// rearranged around: a tab's last pane going is a tab going, which is another
/// change entirely.
const SPARE_PANE: usize = 2;

/// The fewest tabs a session must hold for putting them in another order to
/// mean anything.
const SPARE_TAB: usize = 2;

/// Every kind of change, in the order the generator draws them. The list is
/// the count, so the two cannot drift apart.
const CHANGE_KINDS: &[fn(&mut ModelGenerator, &mut HostModel) -> Vec<Delta>] = &[
    ModelGenerator::add_session,
    ModelGenerator::rename_session,
    ModelGenerator::remove_session,
    ModelGenerator::add_tab,
    ModelGenerator::rename_tab,
    ModelGenerator::remove_tab,
    ModelGenerator::reorder_tabs,
    ModelGenerator::add_pane,
    ModelGenerator::remove_pane,
    ModelGenerator::move_pane,
    ModelGenerator::rearrange,
    ModelGenerator::change_title,
    ModelGenerator::change_directory,
    ModelGenerator::resize_pane,
];

/// How many ways a generated pane goes: closed, exited, signalled.
const REMOVAL_REASONS: u64 = 3;

/// The exit statuses a generated shell reports, from zero upward.
const EXIT_STATUSES: u64 = 256;

/// The signals a generated shell dies of, from zero upward.
const SIGNALS: u64 = 32;

/// A generated sequence of changes, with both ends of it.
#[derive(Clone, Debug)]
pub struct DeltaSequence {
    /// The model the changes start from.
    pub start: HostModel,
    /// The changes, each the deltas of one thing happening, in order.
    pub changes: Vec<Vec<Delta>>,
    /// The model the changes lead to, built directly rather than by applying
    /// them.
    pub finish: HostModel,
}

impl DeltaSequence {
    /// Every delta of every change, in order.
    #[must_use]
    pub fn deltas(&self) -> Vec<Delta> {
        self.changes.iter().flatten().cloned().collect()
    }
}

/// A deterministic source of valid models, changes and registry operations.
#[derive(Debug)]
pub struct ModelGenerator {
    /// The xorshift state.
    state: u64,
    /// The next session id to mint.
    next_session: u64,
    /// The next tab id to mint.
    next_tab: u64,
    /// The next pane id to mint.
    next_pane: u64,
}

impl ModelGenerator {
    /// A generator from a seed. The same seed yields the same everything.
    #[must_use]
    pub fn new(seed: u64) -> ModelGenerator {
        ModelGenerator {
            state: if seed == 0 { ZERO_SEED_STATE } else { seed },
            next_session: 1,
            next_tab: 1,
            next_pane: 1,
        }
    }

    /// The next value of the xorshift.
    fn next(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state.wrapping_shl(FIRST_SHIFT);
        state ^= state.wrapping_shr(SECOND_SHIFT);
        state ^= state.wrapping_shl(THIRD_SHIFT);
        self.state = state;
        state
    }

    /// The next value below `limit`, or zero when `limit` is zero.
    fn below(&mut self, limit: u64) -> u64 {
        self.next().checked_rem(limit).unwrap_or(0)
    }

    /// The next index below `count`, or `None` when there is nothing to pick.
    fn pick(&mut self, count: usize) -> Option<usize> {
        if count == 0 {
            return None;
        }
        usize::try_from(self.below(u64::try_from(count).unwrap_or(1))).ok()
    }

    /// A name nothing else carries, so a rename is visible.
    fn name(&mut self, kind: &str) -> String {
        format!("{kind}-{}", self.next())
    }

    /// A size a pane is given.
    fn size(&mut self) -> (u16, u16) {
        let columns = u16::try_from(self.below(SIZE_SPREAD)).unwrap_or(0);
        let rows = u16::try_from(self.below(SIZE_SPREAD)).unwrap_or(0);
        (
            SMALLEST_COLUMNS.saturating_add(columns),
            SMALLEST_ROWS.saturating_add(rows),
        )
    }

    /// A weight a split hands a child; never zero, which no model allows.
    fn weight(&mut self) -> u32 {
        let drawn = u32::try_from(self.below(WEIGHT_SPREAD)).unwrap_or(0);
        drawn.saturating_add(1)
    }

    /// A direction.
    fn direction(&mut self) -> SplitDirection {
        if self.below(EITHER) == 0 {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        }
    }

    /// A pane nothing else is.
    fn pane(&mut self) -> Pane {
        let id = PaneId(self.next_pane);
        self.next_pane = self.next_pane.saturating_add(1);
        let (columns, rows) = self.size();
        let directory = self.name("directory");
        Pane {
            id,
            title: self.name("title"),
            working_directory: Some(format!("/{directory}")),
            columns,
            rows,
        }
    }

    /// A tab holding `panes` panes, arranged so that no two are the same.
    fn tab(&mut self, panes: usize) -> Tab {
        let id = TabId(self.next_tab);
        self.next_tab = self.next_tab.saturating_add(1);
        let name = self.name("tab");
        let mut held = vec![self.pane()];
        for _index in 1..panes.max(1) {
            held.push(self.pane());
        }
        let layout = self
            .arrange(&held)
            .unwrap_or(LayoutNode::Leaf(PaneId(self.next_pane)));
        Tab {
            id,
            name,
            panes: held,
            layout,
        }
    }

    /// A normalized layout placing exactly these panes, each once, or `None`
    /// when there are none to place.
    fn arrange(&mut self, panes: &[Pane]) -> Option<LayoutNode> {
        let mut placed = panes.first()?.id;
        let mut layout = LayoutNode::Leaf(placed);
        for pane in panes.iter().skip(1) {
            layout = self.split_at(layout, placed, pane.id);
            placed = pane.id;
        }
        Some(layout)
    }

    /// The layout with `target`'s leaf replaced by a split of it and `added`,
    /// normalized, which is the form a model stores.
    fn split_at(&mut self, layout: LayoutNode, target: PaneId, added: PaneId) -> LayoutNode {
        let direction = self.direction();
        let (first, second) = (self.weight(), self.weight());
        let before = self.below(EITHER) == 0;
        let target_child = Weighted {
            node: LayoutNode::Leaf(target),
            weight: first,
        };
        let added_child = Weighted {
            node: LayoutNode::Leaf(added),
            weight: second,
        };
        let children = if before {
            vec![added_child, target_child]
        } else {
            vec![target_child, added_child]
        };
        let mut placed = layout;
        let _found = placed.replace_leaf(
            target,
            LayoutNode::Split {
                direction,
                children,
            },
        );
        placed
    }

    /// A session holding `tabs` tabs, each holding at least one pane.
    fn session(&mut self, tabs: usize) -> Session {
        let id = SessionId(self.next_session);
        self.next_session = self.next_session.saturating_add(1);
        let name = self.name("session");
        let mut held = Vec::new();
        for _index in 0..tabs.max(1) {
            let panes = self.below(u64::try_from(STARTING_PANES).unwrap_or(1));
            held.push(self.tab(usize::try_from(panes).unwrap_or(0).saturating_add(1)));
        }
        Session {
            id,
            name,
            tabs: held,
        }
    }

    /// A model that holds together: at least one session, each with at least
    /// one tab, each with at least one pane.
    #[must_use]
    pub fn model(&mut self) -> HostModel {
        let sessions = self.below(u64::try_from(STARTING_SESSIONS).unwrap_or(1));
        let mut held = Vec::new();
        for _index in 0..usize::try_from(sessions).unwrap_or(0).saturating_add(1) {
            let tabs = self.below(u64::try_from(STARTING_TABS).unwrap_or(1));
            held.push(self.session(usize::try_from(tabs).unwrap_or(0).saturating_add(1)));
        }
        HostModel {
            generation: Generation(self.below(u64::from(u32::MAX))),
            sessions: held,
        }
    }
}

/// The tab at a place: its session's index in the host, and its own in the
/// session.
fn tab_at(model: &HostModel, place: (usize, usize)) -> Option<&Tab> {
    let (session, tab) = place;
    model.sessions.get(session)?.tabs.get(tab)
}

/// The tab at a place, to change.
fn tab_at_mut(model: &mut HostModel, place: (usize, usize)) -> Option<&mut Tab> {
    let (session, tab) = place;
    model.sessions.get_mut(session)?.tabs.get_mut(tab)
}

impl ModelGenerator {
    /// A sequence of at most `count` changes from `start`, with the model they
    /// lead to. A change that finds nothing to act on — a rename with no
    /// session, a move with one tab — is left out rather than made up, so a
    /// sequence may be shorter than asked for.
    #[must_use]
    pub fn changes(&mut self, start: &HostModel, count: usize) -> DeltaSequence {
        let mut finish = start.clone();
        let mut changes = Vec::new();
        for _index in 0..count {
            let change = self.change(&mut finish);
            if !change.is_empty() {
                changes.push(change);
            }
        }
        DeltaSequence {
            start: start.clone(),
            changes,
            finish,
        }
    }

    /// One change, applied directly to the model and described by its deltas.
    fn change(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let count = u64::try_from(CHANGE_KINDS.len()).unwrap_or(1);
        let index = usize::try_from(self.below(count)).unwrap_or(0);
        let deltas = match CHANGE_KINDS.get(index) {
            Some(kind) => kind(self, model),
            None => Vec::new(),
        };
        let advanced = u64::try_from(deltas.len()).unwrap_or(0);
        model.generation = Generation(model.generation.0.saturating_add(advanced));
        deltas
    }

    /// The place of a tab picked at random.
    fn pick_tab(&mut self, model: &HostModel) -> Option<(usize, usize)> {
        let session = self.pick(model.sessions.len())?;
        let tabs = model
            .sessions
            .get(session)
            .map_or(0, |held| held.tabs.len());
        Some((session, self.pick(tabs)?))
    }

    /// The place of a tab holding at least `least` panes.
    fn pick_tab_holding(&mut self, model: &HostModel, least: usize) -> Option<(usize, usize)> {
        let place = self.pick_tab(model)?;
        let panes = tab_at(model, place).map_or(0, |tab| tab.panes.len());
        (panes >= least).then_some(place)
    }

    /// Why a generated pane went.
    fn removal_reason(&mut self) -> RemovalReason {
        match self.below(REMOVAL_REASONS) {
            0 => RemovalReason::Closed,
            1 => RemovalReason::Exited(ExitStatus::Exited(
                i32::try_from(self.below(EXIT_STATUSES)).unwrap_or(0),
            )),
            _other => RemovalReason::Exited(ExitStatus::Signalled(
                i32::try_from(self.below(SIGNALS)).unwrap_or(0),
            )),
        }
    }

    /// A session appears, with its first tab and that tab's first pane.
    fn add_session(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let session = self.session(1);
        model.sessions.push(session.clone());
        vec![Delta::SessionAdded { session }]
    }

    /// A session is renamed.
    fn rename_session(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let name = self.name("session");
        let Some(index) = self.pick(model.sessions.len()) else {
            return Vec::new();
        };
        let Some(held) = model.sessions.get_mut(index) else {
            return Vec::new();
        };
        name.clone_into(&mut held.name);
        vec![Delta::SessionRenamed {
            session: held.id,
            name,
        }]
    }

    /// A session goes, unless it is the only one: a host with no session at
    /// all leaves later changes nothing to act on.
    fn remove_session(&mut self, model: &mut HostModel) -> Vec<Delta> {
        if model.sessions.len() <= 1 {
            return Vec::new();
        }
        let Some(index) = self.pick(model.sessions.len()) else {
            return Vec::new();
        };
        let Some(session) = model.sessions.get(index).map(|held| held.id) else {
            return Vec::new();
        };
        let _removed = model.sessions.remove(index);
        vec![Delta::SessionRemoved { session }]
    }

    /// A tab appears in a session, at a place in its order.
    fn add_tab(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let tab = self.tab(1);
        let Some(index) = self.pick(model.sessions.len()) else {
            return Vec::new();
        };
        let places = model
            .sessions
            .get(index)
            .map_or(0, |held| held.tabs.len())
            .saturating_add(1);
        let place = self.pick(places).unwrap_or(0);
        let Some(held) = model.sessions.get_mut(index) else {
            return Vec::new();
        };
        // `place` is at most the count read above, which is `insert`'s bound.
        held.tabs.insert(place, tab.clone());
        vec![Delta::TabAdded {
            session: held.id,
            tab,
            index: place,
        }]
    }

    /// A tab is renamed.
    fn rename_tab(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let name = self.name("tab");
        let Some(place) = self.pick_tab(model) else {
            return Vec::new();
        };
        let Some(held) = tab_at_mut(model, place) else {
            return Vec::new();
        };
        name.clone_into(&mut held.name);
        vec![Delta::TabRenamed { tab: held.id, name }]
    }

    /// A tab goes, and its session with it when it was the last one.
    fn remove_tab(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let Some(place) = self.pick_tab(model) else {
            return Vec::new();
        };
        let (session, index) = place;
        let Some(tab) = tab_at(model, place).map(|held| held.id) else {
            return Vec::new();
        };
        let Some(holder) = model.sessions.get_mut(session) else {
            return Vec::new();
        };
        let _removed = holder.tabs.remove(index);
        let emptied = holder.tabs.is_empty().then_some(holder.id);
        let mut deltas = vec![Delta::TabRemoved { tab }];
        if let Some(emptied) = emptied {
            let _gone = model.sessions.remove(session);
            deltas.push(Delta::SessionRemoved { session: emptied });
        }
        deltas
    }

    /// A session's tabs are put in another order, carried whole.
    fn reorder_tabs(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let Some(index) = self.pick(model.sessions.len()) else {
            return Vec::new();
        };
        let count = model.sessions.get(index).map_or(0, |held| held.tabs.len());
        if count < SPARE_TAB {
            return Vec::new();
        }
        let Some(rotation) = self.pick(count) else {
            return Vec::new();
        };
        let Some(held) = model.sessions.get_mut(index) else {
            return Vec::new();
        };
        // `rotation` is below the count read above, which is the bound.
        held.tabs.rotate_left(rotation);
        vec![Delta::TabsReordered {
            session: held.id,
            order: held.tabs.iter().map(|tab| tab.id).collect(),
        }]
    }

    /// A pane appears in a tab, and the layout that places it follows.
    fn add_pane(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let Some(place) = self.pick_tab(model) else {
            return Vec::new();
        };
        let added = self.pane();
        let Some(held) = tab_at_mut(model, place) else {
            return Vec::new();
        };
        let tab = held.id;
        held.panes.push(added.clone());
        let panes = held.panes.clone();
        let Some(layout) = self.arrange(&panes) else {
            return Vec::new();
        };
        let Some(arranged) = tab_at_mut(model, place) else {
            return Vec::new();
        };
        arranged.layout = layout.clone();
        vec![
            Delta::PaneAdded { tab, pane: added },
            Delta::LayoutChanged { tab, layout },
        ]
    }

    /// A pane goes from a tab that holds others, and the layout that no longer
    /// places it follows. A tab's last pane is `remove_tab`'s business.
    fn remove_pane(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let Some(place) = self.pick_tab_holding(model, SPARE_PANE) else {
            return Vec::new();
        };
        let count = tab_at(model, place).map_or(0, |tab| tab.panes.len());
        let Some(index) = self.pick(count) else {
            return Vec::new();
        };
        let reason = self.removal_reason();
        let Some(found) = tab_at(model, place) else {
            return Vec::new();
        };
        let tab = found.id;
        let Some(pane) = found.panes.get(index).map(|held| held.id) else {
            return Vec::new();
        };
        let Some(layout) = found.layout.clone().remove_leaf(pane) else {
            return Vec::new();
        };
        let Some(held) = tab_at_mut(model, place) else {
            return Vec::new();
        };
        let _removed = held.panes.remove(index);
        held.layout = layout.clone();
        vec![
            Delta::PaneRemoved { pane, reason },
            Delta::LayoutChanged { tab, layout },
        ]
    }

    /// A pane moves to another tab, and both layouts follow.
    fn move_pane(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let Some(source) = self.pick_tab_holding(model, SPARE_PANE) else {
            return Vec::new();
        };
        let Some(destination) = self.pick_tab(model) else {
            return Vec::new();
        };
        if source == destination {
            return Vec::new();
        }
        let count = tab_at(model, source).map_or(0, |tab| tab.panes.len());
        let Some(index) = self.pick(count) else {
            return Vec::new();
        };
        let Some(from) = tab_at(model, source) else {
            return Vec::new();
        };
        let from_tab = from.id;
        let Some(moved) = from.panes.get(index).cloned() else {
            return Vec::new();
        };
        let pane = moved.id;
        let Some(from_layout) = from.layout.clone().remove_leaf(pane) else {
            return Vec::new();
        };
        let Some(into) = tab_at(model, destination) else {
            return Vec::new();
        };
        let to_tab = into.id;
        let mut panes = into.panes.clone();
        panes.push(moved.clone());
        let Some(to_layout) = self.arrange(&panes) else {
            return Vec::new();
        };
        // Everything that could fail has, so the two mutations below cannot
        // leave the model changed with no delta to describe it.
        let Some(source_tab) = tab_at_mut(model, source) else {
            return Vec::new();
        };
        let _removed = source_tab.panes.remove(index);
        source_tab.layout = from_layout.clone();
        let Some(destination_tab) = tab_at_mut(model, destination) else {
            return Vec::new();
        };
        destination_tab.panes.push(moved);
        destination_tab.layout = to_layout.clone();
        vec![
            Delta::PaneMoved { pane, to_tab },
            Delta::LayoutChanged {
                tab: from_tab,
                layout: from_layout,
            },
            Delta::LayoutChanged {
                tab: to_tab,
                layout: to_layout,
            },
        ]
    }

    /// A tab is arranged another way, over exactly the panes it holds.
    fn rearrange(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let Some(place) = self.pick_tab_holding(model, SPARE_PANE) else {
            return Vec::new();
        };
        let Some(panes) = tab_at(model, place).map(|tab| tab.panes.clone()) else {
            return Vec::new();
        };
        let Some(layout) = self.arrange(&panes) else {
            return Vec::new();
        };
        let Some(held) = tab_at_mut(model, place) else {
            return Vec::new();
        };
        held.layout = layout.clone();
        vec![Delta::LayoutChanged {
            tab: held.id,
            layout,
        }]
    }

    /// The place of a pane picked at random: its tab's place, and its own.
    fn pick_pane(&mut self, model: &HostModel) -> Option<((usize, usize), usize)> {
        let place = self.pick_tab(model)?;
        let count = tab_at(model, place).map_or(0, |tab| tab.panes.len());
        Some((place, self.pick(count)?))
    }

    /// A pane's title changes.
    fn change_title(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let title = self.name("title");
        let Some((place, index)) = self.pick_pane(model) else {
            return Vec::new();
        };
        let Some(held) = tab_at_mut(model, place).and_then(|tab| tab.panes.get_mut(index)) else {
            return Vec::new();
        };
        title.clone_into(&mut held.title);
        vec![Delta::PaneTitle {
            pane: held.id,
            title,
        }]
    }

    /// A pane's shell reports another working directory.
    fn change_directory(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let path = format!("/{}", self.name("directory"));
        let Some((place, index)) = self.pick_pane(model) else {
            return Vec::new();
        };
        let Some(held) = tab_at_mut(model, place).and_then(|tab| tab.panes.get_mut(index)) else {
            return Vec::new();
        };
        held.working_directory = Some(path.clone());
        vec![Delta::PaneWorkingDirectory {
            pane: held.id,
            path,
        }]
    }

    /// A pane is resized.
    fn resize_pane(&mut self, model: &mut HostModel) -> Vec<Delta> {
        let (columns, rows) = self.size();
        let Some((place, index)) = self.pick_pane(model) else {
            return Vec::new();
        };
        let Some(held) = tab_at_mut(model, place).and_then(|tab| tab.panes.get_mut(index)) else {
            return Vec::new();
        };
        held.columns = columns;
        held.rows = rows;
        vec![Delta::PaneResized {
            pane: held.id,
            columns,
            rows,
        }]
    }
}

/// The item a chooser names, wrapping when it names more than there are, and
/// `None` when there is nothing to choose from.
#[must_use]
pub fn chosen<Item>(items: &[Item], chooser: usize) -> Option<&Item> {
    items.get(chooser.checked_rem(items.len())?)
}

/// One thing to ask a registry to do, named by position rather than by
/// identity.
///
/// A registry mints its own ids, so a sequence generated before it runs can
/// only say "the third pane of the second tab". Every chooser is resolved with
/// [`chosen`] against the model as it stands when the operation runs, which
/// wraps rather than refuses, so a sequence applies to any model that holds
/// anything at all — and an operation that finds nothing to act on is skipped
/// by the test driving it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryOperation {
    /// Create a session, with its first tab and that tab's first pane.
    CreateSession {
        /// What to call it.
        name: String,
    },
    /// Create a tab in the chosen session.
    CreateTab {
        /// Which session.
        session: usize,
        /// What to call it.
        name: String,
    },
    /// Create a pane in the chosen tab, beside the chosen one.
    CreatePane {
        /// Which tab, counted across the host.
        tab: usize,
        /// Which of its panes to split.
        target: usize,
        /// Which way to split it.
        direction: SplitDirection,
        /// Whether the new pane goes before the one it splits.
        before: bool,
    },
    /// Close the chosen pane.
    ClosePane {
        /// Which tab, counted across the host.
        tab: usize,
        /// Which of its panes.
        pane: usize,
    },
    /// Move the chosen pane into another tab, beside the chosen one.
    MovePane {
        /// Which tab it is in, counted across the host.
        tab: usize,
        /// Which of its panes.
        pane: usize,
        /// Which tab it goes to, counted across the host.
        to_tab: usize,
        /// Which of that tab's panes it is placed beside.
        target: usize,
        /// Which way to split that one.
        direction: SplitDirection,
        /// Whether it goes before the one it splits.
        before: bool,
    },
    /// Rename the chosen session.
    RenameSession {
        /// Which session.
        session: usize,
        /// What to call it.
        name: String,
    },
    /// Rename the chosen tab.
    RenameTab {
        /// Which tab, counted across the host.
        tab: usize,
        /// What to call it.
        name: String,
    },
    /// Close the chosen tab and every pane in it.
    CloseTab {
        /// Which tab, counted across the host.
        tab: usize,
    },
    /// Close the chosen session and everything under it.
    CloseSession {
        /// Which session.
        session: usize,
    },
    /// Rotate the chosen session's tabs by this much.
    ReorderTabs {
        /// Which session.
        session: usize,
        /// How far to rotate them left.
        rotation: usize,
    },
    /// Arrange the chosen tab's panes another way.
    SetLayout {
        /// Which tab, counted across the host.
        tab: usize,
    },
}

/// Every kind of operation, in the order the generator draws them. The list is
/// the count, so the two cannot drift apart.
const OPERATION_KINDS: &[fn(&mut ModelGenerator) -> RegistryOperation] = &[
    ModelGenerator::create_session,
    ModelGenerator::create_tab,
    ModelGenerator::create_pane,
    ModelGenerator::close_pane,
    ModelGenerator::relocate_pane,
    ModelGenerator::name_session,
    ModelGenerator::name_tab,
    ModelGenerator::close_tab,
    ModelGenerator::close_session,
    ModelGenerator::rotate_tabs,
    ModelGenerator::set_layout,
];

/// The largest position an operation's chooser names; [`chosen`] wraps it, so
/// this only has to be wide enough to reach anything a test builds.
const CHOOSER_SPREAD: u64 = 64;

impl ModelGenerator {
    /// A sequence of registry operations, in the order to run them.
    #[must_use]
    pub fn operations(&mut self, count: usize) -> Vec<RegistryOperation> {
        (0..count).map(|_index| self.operation()).collect()
    }

    /// A chooser, resolved by [`chosen`] against whatever the model holds.
    fn chooser(&mut self) -> usize {
        usize::try_from(self.below(CHOOSER_SPREAD)).unwrap_or(0)
    }

    /// One operation to ask a registry for.
    fn operation(&mut self) -> RegistryOperation {
        let count = u64::try_from(OPERATION_KINDS.len()).unwrap_or(1);
        let index = usize::try_from(self.below(count)).unwrap_or(0);
        match OPERATION_KINDS.get(index) {
            Some(kind) => kind(self),
            None => RegistryOperation::CreateSession {
                name: self.name("session"),
            },
        }
    }

    /// Where a new pane goes: beside which of a tab's panes, which way, and
    /// on which side.
    fn placement(&mut self) -> (usize, SplitDirection, bool) {
        let target = self.chooser();
        let direction = self.direction();
        (target, direction, self.below(EITHER) == 0)
    }

    /// Create a session.
    fn create_session(&mut self) -> RegistryOperation {
        RegistryOperation::CreateSession {
            name: self.name("session"),
        }
    }

    /// Create a tab.
    fn create_tab(&mut self) -> RegistryOperation {
        let session = self.chooser();
        RegistryOperation::CreateTab {
            session,
            name: self.name("tab"),
        }
    }

    /// Create a pane beside another.
    fn create_pane(&mut self) -> RegistryOperation {
        let tab = self.chooser();
        let (target, direction, before) = self.placement();
        RegistryOperation::CreatePane {
            tab,
            target,
            direction,
            before,
        }
    }

    /// Close a pane.
    fn close_pane(&mut self) -> RegistryOperation {
        let tab = self.chooser();
        RegistryOperation::ClosePane {
            tab,
            pane: self.chooser(),
        }
    }

    /// Move a pane into another tab.
    fn relocate_pane(&mut self) -> RegistryOperation {
        let tab = self.chooser();
        let pane = self.chooser();
        let to_tab = self.chooser();
        let (target, direction, before) = self.placement();
        RegistryOperation::MovePane {
            tab,
            pane,
            to_tab,
            target,
            direction,
            before,
        }
    }

    /// Rename a session.
    fn name_session(&mut self) -> RegistryOperation {
        let session = self.chooser();
        RegistryOperation::RenameSession {
            session,
            name: self.name("session"),
        }
    }

    /// Rename a tab.
    fn name_tab(&mut self) -> RegistryOperation {
        let tab = self.chooser();
        RegistryOperation::RenameTab {
            tab,
            name: self.name("tab"),
        }
    }

    /// Close a tab.
    fn close_tab(&mut self) -> RegistryOperation {
        RegistryOperation::CloseTab {
            tab: self.chooser(),
        }
    }

    /// Close a session.
    fn close_session(&mut self) -> RegistryOperation {
        RegistryOperation::CloseSession {
            session: self.chooser(),
        }
    }

    /// Put a session's tabs in another order.
    fn rotate_tabs(&mut self) -> RegistryOperation {
        let session = self.chooser();
        RegistryOperation::ReorderTabs {
            session,
            rotation: self.chooser(),
        }
    }

    /// Arrange a tab another way.
    fn set_layout(&mut self) -> RegistryOperation {
        RegistryOperation::SetLayout {
            tab: self.chooser(),
        }
    }
}
