//! Per-pane history rings indexed by absolute sequence, and the shared budget
//! that evicts from the least recently focused pane first.
//!
//! `ring` is one pane's bounded history; `HistoryBudget` holds every pane's ring
//! and keeps their total within a cap by shrinking the least recently focused
//! pane's ring first, so a hundred idle panes cannot exhaust a small host.

pub mod ring;

use std::collections::VecDeque;

use iznik_protocol::identity::PaneId;

use crate::history::ring::PaneHistory;

/// A pane's history capacity by default: four mebibytes.
pub const DEFAULT_PANE_HISTORY_BYTES: usize = 4 * 1024 * 1024;

/// The whole daemon's history budget by default: two hundred fifty-six
/// mebibytes.
pub const DEFAULT_HISTORY_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// One pane's ring under the budget.
#[derive(Clone, Debug)]
struct Entry {
    /// The pane it belongs to.
    id: PaneId,
    /// Its history ring.
    history: PaneHistory,
}

/// The shared history budget: every pane's ring, kept within a total by
/// shrinking the least recently focused pane's ring first.
#[derive(Clone, Debug)]
pub struct HistoryBudget {
    /// The most history, in bytes, all panes together may hold.
    total: usize,
    /// Each pane's ring.
    entries: Vec<Entry>,
    /// The panes in focus order, least recently focused at the front.
    order: VecDeque<PaneId>,
}

impl HistoryBudget {
    /// A budget of `total` bytes across all panes.
    #[must_use]
    pub fn new(total: usize) -> HistoryBudget {
        HistoryBudget {
            total,
            entries: Vec::new(),
            order: VecDeque::new(),
        }
    }

    /// Adds a pane with a ring of `capacity`, then brings the total within
    /// budget. A new pane is the most recently focused, so it is not the first
    /// evicted.
    pub fn insert(&mut self, pane: PaneId, capacity: usize) {
        // A pane is inserted once, but drop any entry already under this id so a
        // re-insert cannot double-count or shadow it, symmetric with the focus
        // order's de-duplication just below.
        self.entries.retain(|entry| entry.id != pane);
        self.entries.push(Entry {
            id: pane,
            history: PaneHistory::new(capacity),
        });
        self.order.retain(|held| *held != pane);
        self.order.push_back(pane);
        self.enforce();
    }

    /// Records that a pane was focused, making it the most recently focused, so
    /// the panes that were not focused shrink before it.
    pub fn touch(&mut self, pane: PaneId) {
        if self.order.iter().any(|held| *held == pane) {
            self.order.retain(|held| *held != pane);
            self.order.push_back(pane);
        }
    }

    /// Forgets a pane, giving its share back. Without this a closed pane's
    /// entry keeps its bytes committed for ever and, being the least recently
    /// focused thing left, sits ahead of every live pane in the order — so it
    /// is a live pane's ring that is shrunk to pay for a dead one's.
    pub fn remove(&mut self, pane: PaneId) {
        self.entries.retain(|entry| entry.id != pane);
        self.order.retain(|held| *held != pane);
    }

    /// A pane's ring, if it has one.
    #[must_use]
    pub fn history(&self, pane: PaneId) -> Option<&PaneHistory> {
        self.entries
            .iter()
            .find(|entry| entry.id == pane)
            .map(|entry| &entry.history)
    }

    /// A pane's ring for appending, if it has one.
    pub fn history_mut(&mut self, pane: PaneId) -> Option<&mut PaneHistory> {
        self.entries
            .iter_mut()
            .find(|entry| entry.id == pane)
            .map(|entry| &mut entry.history)
    }

    /// The sum of every ring's capacity.
    fn committed(&self) -> usize {
        self.entries
            .iter()
            .fold(0, |sum, entry| sum.saturating_add(entry.history.capacity()))
    }

    /// Shrinks the least recently focused panes' rings until the total is within
    /// budget.
    fn enforce(&mut self) {
        let order: Vec<PaneId> = self.order.iter().copied().collect();
        for pane in order {
            let overage = self.committed().saturating_sub(self.total);
            if overage == 0 {
                break;
            }
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == pane) {
                let reduced = entry.history.capacity().saturating_sub(overage);
                entry.history.set_capacity(reduced);
            }
        }
    }
}
