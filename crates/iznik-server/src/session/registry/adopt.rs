//! Building a registry from a carried record and the masters it names.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use iznik_protocol::identity::{PaneId, TabId};
use iznik_protocol::model::HostModel;
use tokio::sync::{Notify, broadcast};

use super::program::ProgramWatch;
use super::{
    DELTA_BROADCAST_CAPACITY, ENDING_LOOK_INTERVAL, ENDING_LOOKS, Registry, RegistryDefaults,
    Watching,
};
use crate::adopt::{AdoptError, AdoptedState};
use crate::history::{DEFAULT_PANE_HISTORY_BYTES, HistoryBudget};
use crate::pane::Pane;
use crate::pty::spawn::PtyProcess;
use crate::session::remembered::RememberedCommands;
use crate::terminal::mirror::MirrorThread;

/// A failed adoption: the error, the processes not yet built, and the partial
/// registry. The caller keeps both so neither drop signals an inherited child.
pub(super) type Abandoned = (AdoptError, BTreeMap<PaneId, PtyProcess>, Registry);

/// The registry `state` describes.
///
/// # Errors
///
/// [`AdoptError::Master`] when a pane has no process, and [`AdoptError`] from
/// a pane that cannot be built. The processes and the partial registry come
/// back with the error so the caller can keep the children.
///
/// # Panics
///
/// When `defaults.program_interval` is not zero and this is called outside a
/// Tokio runtime.
pub(super) async fn build(
    defaults: RegistryDefaults,
    budget: Arc<Mutex<HistoryBudget>>,
    mirrors: MirrorThread,
    state: &AdoptedState,
    mut processes: BTreeMap<PaneId, PtyProcess>,
) -> Result<Registry, Abandoned> {
    let (deltas, _receiver) = broadcast::channel(DELTA_BROADCAST_CAPACITY);
    let signal = Arc::new(Notify::new());
    let programs = (!defaults.program_interval.is_zero())
        .then(|| ProgramWatch::start(defaults.program_interval, Arc::clone(&signal)));
    let mut registry = Registry {
        model: state.model.clone(),
        panes: BTreeMap::new(),
        watching: BTreeMap::new(),
        next_session: next_id(state.model.sessions.iter().map(|session| session.id.0)),
        next_tab: next_id(session_tabs(&state.model).map(|tab| tab.0)),
        next_pane: next_id(model_panes(&state.model).map(|pane| pane.0)),
        budget,
        mirrors,
        defaults,
        deltas,
        signal,
        programs,
        instance: state.instance,
        build: None,
        remembered: RememberedCommands::default(),
    };
    let panes: Vec<PaneId> = model_panes(&state.model).collect();
    for pane in panes {
        let Some(carried) = state.panes.iter().find(|candidate| candidate.pane == pane) else {
            return Err(abandon(
                registry,
                processes,
                AdoptError::Malformed {
                    detail: format!("pane {} has no carried record", pane.0),
                },
            ));
        };
        let Some(process) = processes.remove(&pane) else {
            return Err(abandon(
                registry,
                processes,
                AdoptError::Master {
                    descriptor: carried.descriptor,
                },
            ));
        };
        let (columns, rows) = pane_size(&state.model, pane);
        let built = match Pane::adopt(
            process,
            DEFAULT_PANE_HISTORY_BYTES,
            &carried.ring,
            carried.sequence,
            columns,
            rows,
            &registry.mirrors,
        )
        .await
        {
            Ok(built) => built,
            Err(error) => {
                return Err(abandon(
                    registry,
                    processes,
                    AdoptError::Io {
                        detail: error.to_string(),
                    },
                ));
            }
        };
        place(&mut registry, pane, built, columns, rows);
    }
    Ok(registry)
}

/// Hands the partial registry and any process not yet built back to the caller.
///
/// Neither is dropped here. Dropping a [`PtyProcess`] would signal a child
/// this process did not spawn, and dropping the registry would kill panes
/// already placed.
fn abandon(
    registry: Registry,
    processes: BTreeMap<PaneId, PtyProcess>,
    error: AdoptError,
) -> Abandoned {
    (error, processes, registry)
}

/// Inserts `pane` the way a spawn does, without minting a new id.
fn place(registry: &mut Registry, id: PaneId, pane: Pane, columns: u16, rows: u16) {
    registry.admit(id);
    let mut changes = pane.state_updates();
    let signal = Arc::clone(&registry.signal);
    let _stirring = tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            signal.notify_one();
        }
        let looking = tokio::time::Instant::now();
        while looking.elapsed() < ENDING_LOOKS {
            signal.notify_waiters();
            signal.notify_one();
            tokio::time::sleep(ENDING_LOOK_INTERVAL).await;
        }
    });
    registry.watching.insert(
        id,
        Watching {
            marks: pane.marks(),
            state: pane.state_updates(),
            size: (columns, rows),
            ended: false,
            unexplained: None,
        },
    );
    let pane = Arc::new(pane);
    if let Some(programs) = &registry.programs {
        programs.watch(id, Arc::clone(&pane));
    }
    registry.panes.insert(id, pane);
    registry.apply_budget();
}

/// The next id after the largest one already used, or one when there is none.
fn next_id(ids: impl Iterator<Item = u64>) -> u64 {
    ids.max().map_or(1, |highest| highest.saturating_add(1))
}

/// Every pane id the model names.
fn model_panes(model: &HostModel) -> impl Iterator<Item = PaneId> + '_ {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .map(|pane| pane.id)
}

/// Every tab id the model names.
fn session_tabs(model: &HostModel) -> impl Iterator<Item = TabId> + '_ {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .map(|tab| tab.id)
}

/// The size the model records for `pane`, or a single cell when it records none.
fn pane_size(model: &HostModel, pane: PaneId) -> (u16, u16) {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .find(|candidate| candidate.id == pane)
        .map_or((1, 1), |found| (found.columns, found.rows))
}
