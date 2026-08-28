//! The host model: sessions holding ordered tabs holding a normalized layout
//! tree of panes, its invariants, and the `Snapshot` payload encoding.
//!
//! Identity is minted once and never reused, and position — a tab's index, a
//! pane's place in a split — is derived from the order of a [`Vec`] and is
//! never a key, so closing a tab leaves every surviving tab's [`TabId`] alone.
//!
//! A tab's layout is kept **normalized**, which is what makes two clients that
//! computed the same arrangement produce the same tree, and the reconciler's
//! equality test mean what it says. A normalized tree holds no split nested in
//! a split of its own direction — such a split is flattened into its parent
//! with every weight scaled so that no child's share of the space changes — no
//! split of a single child, which is replaced by that child, and no split of
//! none, which is dropped wherever there is a parent to drop it into. A
//! split's weights are then divided by the largest factor they share, because
//! weights say only how the space is divided: halves are `1, 1` whether a
//! client called them `1, 1` or `50, 50`, and without that division the factor
//! a flattening multiplies by accumulates forever and eventually saturates,
//! which is a pane the wrong size. [`LayoutNode::normalize`] is a pure,
//! idempotent function the server applies after every operation and to every
//! layout a client submits.
//!
//! One shape survives normalization: a split holding nothing, at the root,
//! where there is no parent to drop it into and no child to collapse it to. It
//! is a layout that places no pane, which is not a layout, and
//! [`HostModel::validate`] refuses it by name.
//!
//! On the wire a model is its fields in declaration order: integers
//! little-endian at their width, a string a four-byte length and then its
//! UTF-8 bytes, an optional string a presence byte and then the string, a
//! sequence a four-byte count and then its elements, and a layout node a tag
//! byte and then its fields. The golden `tests/fixtures/model.jsonl` pins
//! every byte and this code is held to it. Refusals are [`MessageError`]'s,
//! the crate's one vocabulary for what a decoder found; a model payload
//! carries no discriminant, so its refusals name
//! [`crate::message::NO_DISCRIMINANT`].
//!
//! Nesting is bounded by [`MAXIMUM_LAYOUT_DEPTH`], which the decoder refuses
//! on the way down rather than recursing to whatever depth the bytes ask for.
//! That bound is what lets every operation here be written as a plain
//! recursion: no tree this module hands out is deeper than a stack can walk,
//! whatever a peer sends.

use core::fmt::{self, Display, Formatter};
use std::collections::HashSet;

use crate::identity::{Generation, PaneId, SessionId, TabId};
use crate::message::MessageError;
use crate::wire::{ABSENT, PRESENT, Reader, Sink, encode, put_bytes, put_count, unknown};

/// The deepest a layout tree may nest, counting a bare leaf as one.
///
/// A split of the same direction as its parent is flattened away, so depth
/// grows only when a person alternates directions; sixty-four alternations is
/// past any arrangement anybody makes — the innermost pane would be less than
/// a cell wide — and far below the recursion any thread can afford. It is the
/// decoder's bound, and therefore the reason every walk of a tree in this
/// module is safe to write as a recursion.
pub const MAXIMUM_LAYOUT_DEPTH: usize = 64;

/// The discriminants of [`LayoutNode`], in declaration order.
mod layout_tag {
    /// `Split`.
    pub(super) const SPLIT: u8 = 0;
    /// `Leaf`.
    pub(super) const LEAF: u8 = 1;
}

/// The wire values of [`SplitDirection`], in declaration order.
mod direction_tag {
    /// `Horizontal`.
    pub(super) const HORIZONTAL: u8 = 0;
    /// `Vertical`.
    pub(super) const VERTICAL: u8 = 1;
}

/// Everything one host holds, at one version of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostModel {
    /// The version this model is; every change advances it by one.
    pub generation: Generation,
    /// The sessions, in the order a client lists them.
    pub sessions: Vec<Session>,
}

/// A session: a named group of tabs that outlives every client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Minted once by the host and never reused.
    pub id: SessionId,
    /// What a person calls it; never empty.
    pub name: String,
    /// Its tabs, in the order they are shown.
    pub tabs: Vec<Tab>,
}

/// A tab: the panes a person sees at once, and how they are arranged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    /// Minted once by the host and never reused.
    pub id: TabId,
    /// What a person calls it; never empty.
    pub name: String,
    /// Its panes; the layout's leaves are exactly these, each once.
    pub panes: Vec<Pane>,
    /// How they are arranged, normalized.
    pub layout: LayoutNode,
}

/// A pane, as the model knows it: what it is called, where its shell is, and
/// how large it is. The bytes it produces are not model state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    /// Minted once by the host and never reused.
    pub id: PaneId,
    /// What the program running in it last called itself; may be empty,
    /// because a pane has no title until something sets one.
    pub title: String,
    /// Where its shell last said it was, when the shell says so at all.
    pub working_directory: Option<String>,
    /// Its width in cells.
    pub columns: u16,
    /// Its height in cells.
    pub rows: u16,
}

/// How a tab arranges its panes: enough to restore an arrangement,
/// deliberately not enough to compute a cell size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutNode {
    /// Several children sharing the space in one direction, by weight.
    Split {
        /// The direction the children are laid out in.
        direction: SplitDirection,
        /// The children, in order, each with its share.
        children: Vec<Weighted>,
    },
    /// One pane, filling the space it is given.
    Leaf(PaneId),
}

/// A child of a split and its share of the space. Weights are relative to
/// their siblings and mean nothing across splits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Weighted {
    /// The child.
    pub node: LayoutNode,
    /// Its share, relative to its siblings; at least one.
    pub weight: u32,
}

/// Which way a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SplitDirection {
    /// Children side by side, dividing the width.
    Horizontal,
    /// Children stacked, dividing the height.
    Vertical,
}

/// An invariant a model breaks, naming the identity that breaks it.
///
/// [`HostModel::validate`] is for tests and debug builds and never a release
/// path: a model glitch degrades a client, it does not kill the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    /// Two sessions carry one id.
    DuplicateSession {
        /// The id both carry.
        session: SessionId,
    },
    /// Two tabs carry one id, whether or not in the same session.
    DuplicateTab {
        /// The id both carry.
        tab: TabId,
    },
    /// Two panes carry one id, whether or not in the same tab.
    DuplicatePane {
        /// The id both carry.
        pane: PaneId,
    },
    /// A session holds no tabs; a session with nothing in it is closed, not
    /// kept.
    SessionWithoutTabs {
        /// The empty session.
        session: SessionId,
    },
    /// A tab holds no panes; a tab with nothing in it is closed, not kept.
    TabWithoutPanes {
        /// The empty tab.
        tab: TabId,
    },
    /// A tab's layout names a pane the tab does not hold.
    LayoutNamesUnknownPane {
        /// The tab whose layout it is.
        tab: TabId,
        /// The pane it names.
        pane: PaneId,
    },
    /// A tab's layout names one of its panes more than once.
    LayoutRepeatsPane {
        /// The tab whose layout it is.
        tab: TabId,
        /// The pane it names twice.
        pane: PaneId,
    },
    /// A tab holds a pane its layout does not place.
    PaneMissingFromLayout {
        /// The tab holding it.
        tab: TabId,
        /// The pane with nowhere to be.
        pane: PaneId,
    },
    /// A split in a tab's layout holds no children, which is a place nothing
    /// can be drawn in. Normalization drops such a split wherever there is a
    /// parent to drop it into, so one that reaches here is a whole layout
    /// that places no pane.
    EmptySplit {
        /// The tab whose layout it is.
        tab: TabId,
    },
    /// A split gives a child a weight of zero, which is a pane no size.
    ZeroWeight {
        /// The tab whose layout it is.
        tab: TabId,
    },
    /// A tab's layout is not the tree [`LayoutNode::normalize`] would produce,
    /// so two clients holding the same arrangement could disagree about it.
    UnnormalizedLayout {
        /// The tab whose layout it is.
        tab: TabId,
    },
    /// A tab's layout nests deeper than [`MAXIMUM_LAYOUT_DEPTH`].
    LayoutTooDeep {
        /// The tab whose layout it is.
        tab: TabId,
        /// How deeply it nests.
        depth: usize,
    },
    /// A session's name is empty.
    EmptySessionName {
        /// The unnamed session.
        session: SessionId,
    },
    /// A tab's name is empty.
    EmptyTabName {
        /// The unnamed tab.
        tab: TabId,
    },
}

impl Display for ModelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::DuplicateSession { session } => {
                write!(formatter, "two sessions carry id {}", session.0)
            }
            ModelError::DuplicateTab { tab } => {
                write!(formatter, "two tabs carry id {}", tab.0)
            }
            ModelError::DuplicatePane { pane } => {
                write!(formatter, "two panes carry id {}", pane.0)
            }
            ModelError::SessionWithoutTabs { session } => {
                write!(formatter, "session {} holds no tabs", session.0)
            }
            ModelError::TabWithoutPanes { tab } => {
                write!(formatter, "tab {} holds no panes", tab.0)
            }
            ModelError::LayoutNamesUnknownPane { tab, pane } => write!(
                formatter,
                "the layout of tab {} names pane {}, which the tab does not hold",
                tab.0, pane.0
            ),
            ModelError::LayoutRepeatsPane { tab, pane } => write!(
                formatter,
                "the layout of tab {} names pane {} more than once",
                tab.0, pane.0
            ),
            ModelError::PaneMissingFromLayout { tab, pane } => write!(
                formatter,
                "tab {} holds pane {}, which its layout does not place",
                tab.0, pane.0
            ),
            ModelError::EmptySplit { tab } => write!(
                formatter,
                "the layout of tab {} holds a split with no children",
                tab.0
            ),
            ModelError::ZeroWeight { tab } => write!(
                formatter,
                "the layout of tab {} gives a child a weight of zero",
                tab.0
            ),
            ModelError::UnnormalizedLayout { tab } => {
                write!(formatter, "the layout of tab {} is not normalized", tab.0)
            }
            ModelError::LayoutTooDeep { tab, depth } => write!(
                formatter,
                "the layout of tab {} nests {depth} deep, past the {MAXIMUM_LAYOUT_DEPTH} a model holds",
                tab.0
            ),
            ModelError::EmptySessionName { session } => {
                write!(formatter, "session {} has an empty name", session.0)
            }
            ModelError::EmptyTabName { tab } => {
                write!(formatter, "tab {} has an empty name", tab.0)
            }
        }
    }
}

impl core::error::Error for ModelError {}

impl LayoutNode {
    /// The tree in its canonical shape: every child normalized, every split of
    /// this node's own direction flattened into it with its children's weights
    /// scaled so no share of the space changes, every split of no children
    /// dropped, and a split left holding one child replaced by that child.
    ///
    /// Every split's weights are then divided by the largest factor they
    /// share, so that one arrangement is one tree whatever scale a client
    /// expressed it in and the factor a flattening multiplies by does not
    /// accumulate.
    ///
    /// The result holds no same-direction nesting anywhere and no factor a
    /// split's children all share, so normalizing it again changes nothing:
    /// the function is idempotent. Weights saturate at [`u32::MAX`] rather
    /// than wrap, which costs exact proportions only on a tree whose reduced
    /// weights already span four billion — a tree nobody arranged.
    #[must_use]
    pub fn normalize(self) -> LayoutNode {
        let LayoutNode::Split {
            direction,
            children,
        } = self
        else {
            return self;
        };
        let normalized = children
            .into_iter()
            .map(|child| Weighted {
                node: child.node.normalize(),
                weight: child.weight,
            })
            .filter(|child| !is_empty_split(&child.node))
            .collect();
        collapse(direction, reduced(flatten(direction, normalized)))
    }

    /// The panes the tree places, left to right.
    #[must_use]
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut found = Vec::new();
        self.collect_leaves(&mut found);
        found
    }

    /// Appends the panes this node places, left to right.
    fn collect_leaves(&self, into: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Leaf(pane) => into.push(*pane),
            LayoutNode::Split { children, .. } => {
                for child in children {
                    child.node.collect_leaves(into);
                }
            }
        }
    }

    /// How deeply the tree nests: one for a leaf, one more than its deepest
    /// child for a split. A split of no children counts as one, since there
    /// is nothing under it.
    #[must_use]
    pub fn depth(&self) -> usize {
        match self {
            LayoutNode::Leaf(_pane) => 1,
            LayoutNode::Split { children, .. } => children
                .iter()
                .map(|child| child.node.depth())
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        }
    }

    /// Puts `with` where `pane`'s leaf was, and says whether it found one.
    ///
    /// The tree is left normalized when a leaf was replaced — splitting a pane
    /// in the direction its parent already divides is how a same-direction
    /// nesting arises, and flattening it here is what keeps the arrangement
    /// the one every client computes. When no leaf named `pane`, nothing is
    /// touched.
    pub fn replace_leaf(&mut self, pane: PaneId, with: LayoutNode) -> bool {
        let mut pending = Some(with);
        self.substitute(pane, &mut pending);
        if pending.is_some() {
            return false;
        }
        // The placeholder is read by nothing: `normalize` takes the tree by
        // value, and its result is written straight back.
        let tree = core::mem::replace(self, LayoutNode::Leaf(pane));
        *self = tree.normalize();
        true
    }

    /// Moves `pending` into the first leaf naming `pane`, leaving it `None`
    /// when it found one and untouched when it did not. A tree
    /// [`HostModel::validate`] accepts names a pane once, so "the first" is
    /// "the only" wherever the caller has a model that holds together.
    fn substitute(&mut self, pane: PaneId, pending: &mut Option<LayoutNode>) {
        match self {
            LayoutNode::Leaf(named) if *named == pane => {
                if let Some(node) = pending.take() {
                    *self = node;
                }
            }
            LayoutNode::Leaf(_named) => {}
            LayoutNode::Split { children, .. } => {
                for child in children {
                    if pending.is_none() {
                        return;
                    }
                    child.node.substitute(pane, pending);
                }
            }
        }
    }

    /// The tree without `pane`, normalized, or `None` when no pane is left to
    /// place — which is the tab's last pane closing, and the caller's cue to
    /// close the tab. A pane the tree does not hold leaves every other leaf
    /// exactly where it was.
    #[must_use]
    pub fn remove_leaf(self, pane: PaneId) -> Option<LayoutNode> {
        let normalized = self.prune(pane)?.normalize();
        if normalized.leaves().is_empty() {
            None
        } else {
            Some(normalized)
        }
    }

    /// The node with every leaf naming `pane` gone, or `None` when the node
    /// was that leaf itself.
    fn prune(self, pane: PaneId) -> Option<LayoutNode> {
        match self {
            LayoutNode::Leaf(named) if named == pane => None,
            LayoutNode::Leaf(named) => Some(LayoutNode::Leaf(named)),
            LayoutNode::Split {
                direction,
                children,
            } => Some(LayoutNode::Split {
                direction,
                children: children
                    .into_iter()
                    .filter_map(|child| {
                        Some(Weighted {
                            node: child.node.prune(pane)?,
                            weight: child.weight,
                        })
                    })
                    .collect(),
            }),
        }
    }
}

/// Whether a node is a split holding nothing: a shape nothing can render,
/// which normalization drops rather than keeps, so that a tree carrying one
/// is not mistaken for the canonical arrangement of its remaining panes.
fn is_empty_split(node: &LayoutNode) -> bool {
    matches!(node, LayoutNode::Split { children, .. } if children.is_empty())
}

/// The weights of a split's children added up, saturating.
fn weight_total(children: &[Weighted]) -> u32 {
    children
        .iter()
        .fold(0, |total, child| total.saturating_add(child.weight))
}

/// Every same-direction child's children lifted into their parent's place,
/// with weights that leave each pane the share of the space it had.
///
/// Lifting a child of weight `parent` whose own children weigh `inner` in
/// total gives each of those children `parent × inner_child`, and every other
/// child of the parent a factor of `inner` — so the scale the whole list is
/// multiplied by is the product of every lifted child's total, and a lifted
/// child's own children take that product divided by their total.
fn flatten(direction: SplitDirection, children: Vec<Weighted>) -> Vec<Weighted> {
    let scale = children.iter().fold(1_u32, |scale, child| {
        match lifted_children(direction, &child.node) {
            Some(grandchildren) => scale.saturating_mul(weight_total(grandchildren).max(1)),
            None => scale,
        }
    });
    let mut flattened = Vec::with_capacity(children.len());
    for child in children {
        push_lifted(&mut flattened, direction, child, scale);
    }
    flattened
}

/// The children a child contributes to its parent when it is a split of the
/// parent's own direction, and `None` when it is anything else.
fn lifted_children(direction: SplitDirection, node: &LayoutNode) -> Option<&[Weighted]> {
    match node {
        LayoutNode::Split {
            direction: inner,
            children,
        } if *inner == direction => Some(children),
        _other => None,
    }
}

/// Appends one child of a split, lifted if it is a split of the same
/// direction and scaled if it is not.
fn push_lifted(into: &mut Vec<Weighted>, direction: SplitDirection, child: Weighted, scale: u32) {
    let Weighted { node, weight } = child;
    let LayoutNode::Split {
        direction: inner,
        children: grandchildren,
    } = node
    else {
        into.push(Weighted {
            node,
            weight: weight.saturating_mul(scale),
        });
        return;
    };
    if inner != direction {
        into.push(Weighted {
            node: LayoutNode::Split {
                direction: inner,
                children: grandchildren,
            },
            weight: weight.saturating_mul(scale),
        });
        return;
    }
    // A split whose children all weigh nothing has no shares to preserve, so
    // every product below is zero and the share the division would have
    // yielded is not read.
    let share = scale
        .checked_div(weight_total(&grandchildren))
        .unwrap_or(scale);
    for grandchild in grandchildren {
        into.push(Weighted {
            node: grandchild.node,
            weight: weight
                .saturating_mul(grandchild.weight)
                .saturating_mul(share),
        });
    }
}

/// The greatest common divisor of two weights, by Euclid. A weight of zero
/// divides nothing and so contributes no factor: `common_divisor(0, n)` is
/// `n`, and children that all weigh nothing share no factor at all.
fn common_divisor(left: u32, right: u32) -> u32 {
    let mut larger = left;
    let mut smaller = right;
    while smaller != 0 {
        let remainder = larger.checked_rem(smaller).unwrap_or(0);
        larger = smaller;
        smaller = remainder;
    }
    larger
}

/// The children with every weight divided by the largest factor they share.
/// Weights say only how the space is divided, so this loses nothing and is
/// what makes one arrangement one tree; children that all weigh nothing have
/// no factor to take out and are left as they are.
fn reduced(children: Vec<Weighted>) -> Vec<Weighted> {
    let divisor = children
        .iter()
        .fold(0, |divisor, child| common_divisor(divisor, child.weight));
    if divisor <= 1 {
        return children;
    }
    children
        .into_iter()
        .map(|child| Weighted {
            weight: child.weight.checked_div(divisor).unwrap_or(child.weight),
            node: child.node,
        })
        .collect()
}

/// A split of one child is that child; a split of none stays as it is,
/// because there is nothing to replace it with.
fn collapse(direction: SplitDirection, children: Vec<Weighted>) -> LayoutNode {
    match <[Weighted; 1]>::try_from(children) {
        Ok([only]) => only.node,
        Err(children) => LayoutNode::Split {
            direction,
            children,
        },
    }
}

/// The ids a validation has already met, so that "unique across the host" is
/// one pass rather than a search per identity.
#[derive(Debug, Default)]
struct SeenIds {
    /// Every session id met so far.
    sessions: HashSet<SessionId>,
    /// Every tab id met so far.
    tabs: HashSet<TabId>,
    /// Every pane id met so far.
    panes: HashSet<PaneId>,
}

impl HostModel {
    /// Confirms the model holds together.
    ///
    /// This is for tests and debug builds and never a release path: a model
    /// glitch degrades a client, it does not kill the server.
    ///
    /// # Errors
    ///
    /// One [`ModelError`] per invariant, naming the identity that breaks it:
    /// ids are unique across the host; every session holds at least one tab
    /// and every tab at least one pane; a tab's layout leaves are exactly its
    /// panes, each once; every weight is at least one; the layout is
    /// normalized and nests no deeper than [`MAXIMUM_LAYOUT_DEPTH`]; session
    /// and tab names are not empty.
    pub fn validate(&self) -> Result<(), ModelError> {
        let mut seen = SeenIds::default();
        for session in &self.sessions {
            validate_session(&mut seen, session)?;
        }
        Ok(())
    }
}

/// Confirms one session holds together, and records its identities.
///
/// # Errors
///
/// The [`ModelError`] of the first invariant the session or anything under it
/// breaks.
fn validate_session(seen: &mut SeenIds, session: &Session) -> Result<(), ModelError> {
    if !seen.sessions.insert(session.id) {
        return Err(ModelError::DuplicateSession {
            session: session.id,
        });
    }
    if session.name.is_empty() {
        return Err(ModelError::EmptySessionName {
            session: session.id,
        });
    }
    if session.tabs.is_empty() {
        return Err(ModelError::SessionWithoutTabs {
            session: session.id,
        });
    }
    for tab in &session.tabs {
        validate_tab(seen, tab)?;
    }
    Ok(())
}

/// Confirms one tab holds together, and records its identities.
///
/// # Errors
///
/// The [`ModelError`] of the first invariant the tab or its layout breaks.
fn validate_tab(seen: &mut SeenIds, tab: &Tab) -> Result<(), ModelError> {
    if !seen.tabs.insert(tab.id) {
        return Err(ModelError::DuplicateTab { tab: tab.id });
    }
    if tab.name.is_empty() {
        return Err(ModelError::EmptyTabName { tab: tab.id });
    }
    if tab.panes.is_empty() {
        return Err(ModelError::TabWithoutPanes { tab: tab.id });
    }
    for pane in &tab.panes {
        if !seen.panes.insert(pane.id) {
            return Err(ModelError::DuplicatePane { pane: pane.id });
        }
    }
    validate_layout(tab)
}

/// Confirms a tab's layout places exactly its panes, each once, in a tree
/// that is normalized, weighted and no deeper than a model holds.
///
/// # Errors
///
/// The [`ModelError`] of the first of those the layout breaks.
fn validate_layout(tab: &Tab) -> Result<(), ModelError> {
    let depth = tab.layout.depth();
    if depth > MAXIMUM_LAYOUT_DEPTH {
        return Err(ModelError::LayoutTooDeep { tab: tab.id, depth });
    }
    if has_empty_split(&tab.layout) {
        return Err(ModelError::EmptySplit { tab: tab.id });
    }
    if has_zero_weight(&tab.layout) {
        return Err(ModelError::ZeroWeight { tab: tab.id });
    }
    let held: HashSet<PaneId> = tab.panes.iter().map(|pane| pane.id).collect();
    let mut placed = HashSet::new();
    for leaf in tab.layout.leaves() {
        if !held.contains(&leaf) {
            return Err(ModelError::LayoutNamesUnknownPane {
                tab: tab.id,
                pane: leaf,
            });
        }
        if !placed.insert(leaf) {
            return Err(ModelError::LayoutRepeatsPane {
                tab: tab.id,
                pane: leaf,
            });
        }
    }
    for pane in &tab.panes {
        if !placed.contains(&pane.id) {
            return Err(ModelError::PaneMissingFromLayout {
                tab: tab.id,
                pane: pane.id,
            });
        }
    }
    if tab.layout.clone().normalize() == tab.layout {
        Ok(())
    } else {
        Err(ModelError::UnnormalizedLayout { tab: tab.id })
    }
}

/// Whether any split in the tree holds no children at all.
fn has_empty_split(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Leaf(_pane) => false,
        LayoutNode::Split { children, .. } => {
            children.is_empty() || children.iter().any(|child| has_empty_split(&child.node))
        }
    }
}

/// Whether any split in the tree gives a child a weight of zero.
fn has_zero_weight(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Leaf(_pane) => false,
        LayoutNode::Split { children, .. } => children
            .iter()
            .any(|child| child.weight == 0 || has_zero_weight(&child.node)),
    }
}

/// Appends a host model.
fn put_host_model(sink: &mut dyn Sink, model: &HostModel) {
    sink.put(&model.generation.0.to_le_bytes());
    put_count(sink, model.sessions.len());
    for session in &model.sessions {
        put_session(sink, session);
    }
}

/// Appends a session and its tabs.
fn put_session(sink: &mut dyn Sink, session: &Session) {
    sink.put(&session.id.0.to_le_bytes());
    put_bytes(sink, session.name.as_bytes());
    put_count(sink, session.tabs.len());
    for tab in &session.tabs {
        put_tab(sink, tab);
    }
}

/// Appends a tab, its panes and its layout.
fn put_tab(sink: &mut dyn Sink, tab: &Tab) {
    sink.put(&tab.id.0.to_le_bytes());
    put_bytes(sink, tab.name.as_bytes());
    put_count(sink, tab.panes.len());
    for pane in &tab.panes {
        put_pane(sink, pane);
    }
    put_layout(sink, &tab.layout);
}

/// Appends a pane.
fn put_pane(sink: &mut dyn Sink, pane: &Pane) {
    sink.put(&pane.id.0.to_le_bytes());
    put_bytes(sink, pane.title.as_bytes());
    match pane.working_directory.as_deref() {
        None => sink.put(&[ABSENT]),
        Some(path) => {
            sink.put(&[PRESENT]);
            put_bytes(sink, path.as_bytes());
        }
    }
    sink.put(&pane.columns.to_le_bytes());
    sink.put(&pane.rows.to_le_bytes());
}

/// Appends a layout node: its tag and then its fields, a child's weight
/// after the child itself.
fn put_layout(sink: &mut dyn Sink, node: &LayoutNode) {
    match node {
        LayoutNode::Split {
            direction,
            children,
        } => {
            let direction = match direction {
                SplitDirection::Horizontal => direction_tag::HORIZONTAL,
                SplitDirection::Vertical => direction_tag::VERTICAL,
            };
            sink.put(&[layout_tag::SPLIT, direction]);
            put_count(sink, children.len());
            for child in children {
                put_layout(sink, &child.node);
                sink.put(&child.weight.to_le_bytes());
            }
        }
        LayoutNode::Leaf(pane) => {
            sink.put(&[layout_tag::LEAF]);
            sink.put(&pane.0.to_le_bytes());
        }
    }
}

/// The `Snapshot` payload for a model.
///
/// # Errors
///
/// [`MessageError::LayoutTooDeep`] when a tab's layout nests past
/// [`MAXIMUM_LAYOUT_DEPTH`], so that nothing this encoder produces is
/// something [`decode_host_model`] refuses; [`MessageError::Oversize`] when
/// the encoding would not fit a frame, measured before anything is allocated
/// for it.
pub fn encode_host_model(model: &HostModel) -> Result<Vec<u8>, MessageError> {
    let too_deep = model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .any(|tab| tab.layout.depth() > MAXIMUM_LAYOUT_DEPTH);
    if too_deep {
        return Err(MessageError::LayoutTooDeep {
            limit: MAXIMUM_LAYOUT_DEPTH,
        });
    }
    encode(|sink| put_host_model(sink, model))
}

/// The model a `Snapshot` payload holds.
///
/// The bytes are not trusted: a count is read as far as the bytes go rather
/// than allocated for, and a layout is refused on the way down at
/// [`MAXIMUM_LAYOUT_DEPTH`] rather than recursed to whatever depth it asks
/// for. What comes back is a well-formed model, not necessarily a valid one —
/// [`HostModel::validate`] is what says that.
///
/// # Errors
///
/// [`MessageError::Truncated`] when a field ends early,
/// [`MessageError::TrailingBytes`] when bytes follow the last session,
/// [`MessageError::Utf8`] when a name is not UTF-8,
/// [`MessageError::UnknownDiscriminant`] for a layout tag, split direction or
/// presence byte no variant claims, and [`MessageError::LayoutTooDeep`] for a
/// layout nested past [`MAXIMUM_LAYOUT_DEPTH`]. Every refusal names
/// [`crate::message::NO_DISCRIMINANT`]: a model payload carries none.
pub fn decode_host_model(bytes: &[u8]) -> Result<HostModel, MessageError> {
    let mut reader = Reader::payload(bytes);
    let model = read_host_model(&mut reader)?;
    reader.finish()?;
    Ok(model)
}

/// The host model at the reader.
///
/// # Errors
///
/// The refusals [`decode_host_model`] documents.
fn read_host_model(reader: &mut Reader<'_>) -> Result<HostModel, MessageError> {
    let generation = Generation(u64::from_le_bytes(reader.array()?));
    let count = reader.count()?;
    let mut sessions = Vec::new();
    for _index in 0..count {
        sessions.push(read_session(reader)?);
    }
    Ok(HostModel {
        generation,
        sessions,
    })
}

/// The session at the reader.
///
/// # Errors
///
/// The refusals [`decode_host_model`] documents.
fn read_session(reader: &mut Reader<'_>) -> Result<Session, MessageError> {
    let id = SessionId(u64::from_le_bytes(reader.array()?));
    let name = reader.string()?;
    let count = reader.count()?;
    let mut tabs = Vec::new();
    for _index in 0..count {
        tabs.push(read_tab(reader)?);
    }
    Ok(Session { id, name, tabs })
}

/// The tab at the reader.
///
/// # Errors
///
/// The refusals [`decode_host_model`] documents.
fn read_tab(reader: &mut Reader<'_>) -> Result<Tab, MessageError> {
    let id = TabId(u64::from_le_bytes(reader.array()?));
    let name = reader.string()?;
    let count = reader.count()?;
    let mut panes = Vec::new();
    for _index in 0..count {
        panes.push(read_pane(reader)?);
    }
    let layout = read_layout(reader, 1)?;
    Ok(Tab {
        id,
        name,
        panes,
        layout,
    })
}

/// The pane at the reader.
///
/// # Errors
///
/// The refusals [`decode_host_model`] documents.
fn read_pane(reader: &mut Reader<'_>) -> Result<Pane, MessageError> {
    let id = PaneId(u64::from_le_bytes(reader.array()?));
    let title = reader.string()?;
    let working_directory = if reader.flag()? {
        Some(reader.string()?)
    } else {
        None
    };
    let columns = u16::from_le_bytes(reader.array()?);
    let rows = u16::from_le_bytes(reader.array()?);
    Ok(Pane {
        id,
        title,
        working_directory,
        columns,
        rows,
    })
}

/// The layout node at `depth`, refusing a tree that nests past
/// [`MAXIMUM_LAYOUT_DEPTH`] before recursing that far.
///
/// # Errors
///
/// The refusals [`decode_host_model`] documents.
fn read_layout(reader: &mut Reader<'_>, depth: usize) -> Result<LayoutNode, MessageError> {
    if depth > MAXIMUM_LAYOUT_DEPTH {
        return Err(MessageError::LayoutTooDeep {
            limit: MAXIMUM_LAYOUT_DEPTH,
        });
    }
    match reader.byte()? {
        layout_tag::SPLIT => {
            let direction = match reader.byte()? {
                direction_tag::HORIZONTAL => SplitDirection::Horizontal,
                direction_tag::VERTICAL => SplitDirection::Vertical,
                other => return Err(unknown(other)),
            };
            let count = reader.count()?;
            let mut children = Vec::new();
            for _index in 0..count {
                let node = read_layout(reader, depth.saturating_add(1))?;
                let weight = u32::from_le_bytes(reader.array()?);
                children.push(Weighted { node, weight });
            }
            Ok(LayoutNode::Split {
                direction,
                children,
            })
        }
        layout_tag::LEAF => Ok(LayoutNode::Leaf(PaneId(u64::from_le_bytes(
            reader.array()?,
        )))),
        other => Err(unknown(other)),
    }
}
