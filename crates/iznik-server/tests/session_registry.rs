//! The registry held to the property every client depends on — the deltas it
//! emits rebuild the model it holds — and to the order, the cascade and the
//! arrangement rules that property is made of.
//!
//! Its panes run `sh`, which starts and ends at once: a login shell would draw
//! a prompt asynchronously and a thousand operations would take minutes. The
//! one case that needs a shell with integration is the ingestion case, and it
//! says so.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::command::Placement;
use iznik_protocol::delta::{Delta, RemovalReason};
use iznik_protocol::identity::{PaneId, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, SplitDirection, Weighted};
use iznik_protocol::reconcile::apply;
use iznik_server::history::ring::PaneHistory;
use iznik_server::history::{
    DEFAULT_HISTORY_BUDGET_BYTES, DEFAULT_PANE_HISTORY_BYTES, HistoryBudget,
};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{
    DELTA_BROADCAST_CAPACITY, Numbered, Registry, RegistryDefaults,
};
use iznik_server::terminal::mirror::{MirrorError, MirrorThread};
use iznik_testkit::generate::{ModelGenerator, RegistryOperation, chosen};
use tokio::sync::broadcast;

/// How many generated operation sequences the convergence property is checked
/// over.
const SEQUENCES: usize = 200;

/// How many operations each sequence carries.
const OPERATIONS: usize = 20;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_1201_0003;

/// The size every pane in these cases is created at.
const COLUMNS: u16 = 80;

/// The height every pane in these cases is created at.
const ROWS: u16 = 24;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a case waits for a shell to say something, in short looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How many looks a case takes before it gives up on a shell.
const POLL_ATTEMPTS: usize = 250;

/// A registry whose panes run the given program.
///
/// # Errors
///
/// When the mirror thread cannot be started.
fn registry_running(program: Program) -> Result<Registry, MirrorError> {
    let mirrors = MirrorThread::start()?;
    let budget = Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES)));
    Ok(Registry::new(
        RegistryDefaults {
            program,
            terminfo_directory: None,
        },
        budget,
        mirrors,
    ))
}

/// A registry whose panes run `sh`.
///
/// # Errors
///
/// When the mirror thread cannot be started.
fn registry() -> Result<Registry, MirrorError> {
    registry_running(Program::Command {
        path: "sh".into(),
        arguments: Vec::new(),
    })
}

/// A registry whose panes run `sh` and take their history out of a budget the
/// caller keeps a handle on.
///
/// # Errors
///
/// When the mirror thread cannot be started.
fn registry_sharing(budget: Arc<Mutex<HistoryBudget>>) -> Result<Registry, MirrorError> {
    let mirrors = MirrorThread::start()?;
    Ok(Registry::new(
        RegistryDefaults {
            program: Program::Command {
                path: "sh".into(),
                arguments: Vec::new(),
            },
            terminfo_directory: None,
        },
        budget,
        mirrors,
    ))
}

/// Everything the registry has said that the receiver has not taken.
fn drain(receiver: &mut broadcast::Receiver<Numbered<Delta>>) -> Vec<Numbered<Delta>> {
    let mut seen = Vec::new();
    while let Ok(numbered) = receiver.try_recv() {
        seen.push(numbered);
    }
    seen
}

/// The kinds of the deltas, in order, so a case can say what an operation
/// emitted without repeating every field.
fn kinds(seen: &[Numbered<Delta>]) -> Vec<String> {
    seen.iter()
        .map(|numbered| {
            let named = format!("{:?}", numbered.value);
            named
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// Every tab the model holds, with the session that holds it.
fn tabs(model: &HostModel) -> Vec<(SessionId, TabId)> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter().map(|tab| (session.id, tab.id)))
        .collect()
}

/// Every pane the model holds, with the tab that holds it.
fn panes(model: &HostModel) -> Vec<(TabId, PaneId)> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter().map(|pane| (tab.id, pane.id)))
        .collect()
}

/// The layout of a tab the model holds.
fn layout_of(model: &HostModel, tab: TabId) -> Option<LayoutNode> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .find(|held| held.id == tab)
        .map(|held| held.layout.clone())
}

/// Runs one generated operation against the registry, resolving its choosers
/// against the model as it stands. An operation with nothing to act on, or one
/// the registry refuses, is a step that changed nothing — which is a case the
/// property has to survive too.
async fn run(registry: &mut Registry, operation: &RegistryOperation) {
    let model = registry.snapshot();
    let sessions: Vec<SessionId> = model.sessions.iter().map(|session| session.id).collect();
    let held_tabs = tabs(&model);
    let held_panes = panes(&model);
    match operation {
        RegistryOperation::CreateSession { name } => {
            let _made = registry
                .create_session(name.clone(), COLUMNS, ROWS, None)
                .await;
        }
        RegistryOperation::CreateTab { session, name } => {
            if let Some(chosen_session) = chosen(&sessions, *session) {
                let _made = registry
                    .create_tab(*chosen_session, name.clone(), COLUMNS, ROWS, None)
                    .await;
            }
        }
        RegistryOperation::CreatePane {
            tab,
            target,
            direction,
            before,
        } => {
            if let Some((_session, chosen_tab)) = chosen(&held_tabs, *tab) {
                let inside: Vec<PaneId> = held_panes
                    .iter()
                    .filter(|(held, _pane)| held == chosen_tab)
                    .map(|(_held, pane)| *pane)
                    .collect();
                if let Some(chosen_pane) = chosen(&inside, *target) {
                    let placement = Placement {
                        target: *chosen_pane,
                        direction: *direction,
                        before: *before,
                    };
                    let _made = registry
                        .create_pane(*chosen_tab, placement, COLUMNS, ROWS, None)
                        .await;
                }
            }
        }
        _other => change(registry, operation, &model),
    }
}

/// Runs one generated operation that changes what is already there.
fn change(registry: &mut Registry, operation: &RegistryOperation, model: &HostModel) {
    let sessions: Vec<SessionId> = model.sessions.iter().map(|session| session.id).collect();
    let held_tabs = tabs(model);
    let held_panes = panes(model);
    match operation {
        RegistryOperation::CreateSession { .. }
        | RegistryOperation::CreateTab { .. }
        | RegistryOperation::CreatePane { .. } => {}
        RegistryOperation::ClosePane { tab: _tab, pane } => {
            if let Some((_held, chosen_pane)) = chosen(&held_panes, *pane) {
                let _closed = registry.close_pane(*chosen_pane);
            }
        }
        RegistryOperation::MovePane {
            tab: _tab,
            pane,
            to_tab,
            target,
            direction,
            before,
        } => {
            if let (Some((_from, chosen_pane)), Some((_session, chosen_tab))) =
                (chosen(&held_panes, *pane), chosen(&held_tabs, *to_tab))
            {
                let inside: Vec<PaneId> = held_panes
                    .iter()
                    .filter(|(holder, _held)| holder == chosen_tab)
                    .map(|(_holder, held)| *held)
                    .collect();
                if let Some(chosen_target) = chosen(&inside, *target) {
                    let placement = Placement {
                        target: *chosen_target,
                        direction: *direction,
                        before: *before,
                    };
                    let _moved = registry.move_pane(*chosen_pane, *chosen_tab, placement);
                }
            }
        }
        RegistryOperation::RenameSession { session, name } => {
            if let Some(chosen_session) = chosen(&sessions, *session) {
                let _renamed = registry.rename_session(*chosen_session, name.clone());
            }
        }
        RegistryOperation::RenameTab { tab, name } => {
            if let Some((_session, chosen_tab)) = chosen(&held_tabs, *tab) {
                let _renamed = registry.rename_tab(*chosen_tab, name.clone());
            }
        }
        RegistryOperation::CloseTab { tab } => {
            if let Some((_session, chosen_tab)) = chosen(&held_tabs, *tab) {
                let _closed = registry.close_tab(*chosen_tab);
            }
        }
        RegistryOperation::CloseSession { session } => {
            if let Some(chosen_session) = chosen(&sessions, *session) {
                let _closed = registry.close_session(*chosen_session);
            }
        }
        RegistryOperation::ReorderTabs { session, rotation } => {
            if let Some(chosen_session) = chosen(&sessions, *session) {
                let order = rotated(model, *chosen_session, *rotation);
                let _ordered = registry.reorder_tabs(*chosen_session, order);
            }
        }
        RegistryOperation::ReorderSessions { rotation } => {
            let order = rotated_sessions(model, *rotation);
            let _ordered = registry.reorder_sessions(order);
        }
        RegistryOperation::SetLayout { tab } => {
            if let Some((_session, chosen_tab)) = chosen(&held_tabs, *tab)
                && let Some(layout) = rearranged(model, *chosen_tab)
            {
                let _arranged = registry.set_layout(*chosen_tab, layout);
            }
        }
    }
}

/// The host's sessions rotated by this much, which is a permutation of them.
fn rotated_sessions(model: &HostModel, rotation: usize) -> Vec<SessionId> {
    let mut order: Vec<SessionId> = model.sessions.iter().map(|session| session.id).collect();
    if !order.is_empty() {
        let places = rotation.checked_rem(order.len()).unwrap_or(0);
        order.rotate_left(places);
    }
    order
}

/// A session's tabs rotated by this much, which is a permutation of them.
fn rotated(model: &HostModel, session: SessionId, rotation: usize) -> Vec<TabId> {
    let mut order: Vec<TabId> = model
        .sessions
        .iter()
        .find(|held| held.id == session)
        .map(|held| held.tabs.iter().map(|tab| tab.id).collect())
        .unwrap_or_default();
    if !order.is_empty() {
        let places = rotation.checked_rem(order.len()).unwrap_or(0);
        order.rotate_left(places);
    }
    order
}

/// A tab's panes arranged another way: a flat split of all of them.
fn rearranged(model: &HostModel, tab: TabId) -> Option<LayoutNode> {
    let inside: Vec<PaneId> = panes(model)
        .into_iter()
        .filter(|(held, _pane)| *held == tab)
        .map(|(_held, pane)| pane)
        .collect();
    let (first, rest) = inside.split_first()?;
    if rest.is_empty() {
        return Some(LayoutNode::Leaf(*first));
    }
    Some(LayoutNode::Split {
        direction: SplitDirection::Vertical,
        children: inside
            .iter()
            .map(|pane| Weighted {
                node: LayoutNode::Leaf(*pane),
                weight: 1,
            })
            .collect(),
    })
}

/// Each operation emits exactly the deltas the architecture lists, in that
/// order, with consecutive generations.
///
/// # Panics
///
/// When an operation says something else, or says it out of order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_each_operation_emits_its_deltas_in_order() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let mut deltas = registry.deltas();

        let session = registry
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        assert_eq!(
            kinds(&drain(&mut deltas)),
            ["SessionAdded"],
            "create_session"
        );

        let tab = registry
            .create_tab(session, "edit".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a tab");
        assert_eq!(kinds(&drain(&mut deltas)), ["TabAdded"], "create_tab");

        let first = panes(&registry.snapshot())
            .into_iter()
            .find(|(held, _pane)| *held == tab)
            .map(|(_held, pane)| pane)
            .expect("the tab's first pane");
        let placement = Placement {
            target: first,
            direction: SplitDirection::Horizontal,
            before: false,
        };
        let _made = registry
            .create_pane(tab, placement, COLUMNS, ROWS, None)
            .await
            .expect("a pane");
        let emitted = drain(&mut deltas);
        assert_eq!(
            kinds(&emitted),
            ["PaneAdded", "LayoutChanged"],
            "create_pane"
        );
        let numbers: Vec<u64> = emitted
            .iter()
            .map(|numbered| numbered.generation.0)
            .collect();
        assert_eq!(
            numbers,
            vec![
                numbers.first().copied().unwrap_or(0),
                numbers.first().copied().unwrap_or(0).saturating_add(1)
            ],
            "one generation per delta"
        );

        registry
            .rename_tab(tab, "renamed".to_owned())
            .expect("a rename");
        assert_eq!(kinds(&drain(&mut deltas)), ["TabRenamed"], "rename_tab");

        registry.close_tab(tab).expect("a close");
        assert_eq!(kinds(&drain(&mut deltas)), ["TabRemoved"], "close_tab");

        registry.close_session(session).expect("a close");
        assert_eq!(
            kinds(&drain(&mut deltas)),
            ["SessionRemoved"],
            "close_session"
        );

        let one = registry
            .create_session("one".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        drop(drain(&mut deltas));
        let two = registry
            .create_session("two".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        drop(drain(&mut deltas));
        registry
            .reorder_sessions(vec![two, one])
            .expect("a reorder");
        assert_eq!(
            kinds(&drain(&mut deltas)),
            ["SessionsReordered"],
            "reorder_sessions"
        );
        assert_eq!(
            registry
                .snapshot()
                .sessions
                .iter()
                .map(|held| held.id)
                .collect::<Vec<_>>(),
            [two, one],
            "the order is the one carried"
        );
        registry.close_session(two).expect("a close");
        drop(drain(&mut deltas));
        registry.close_session(one).expect("a close");
        drop(drain(&mut deltas));
        assert_eq!(registry.snapshot().sessions.len(), 0, "nothing is left");
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the delta-order case finishes");
}

/// The deltas the registry emits rebuild the model it holds: over two hundred
/// generated sequences, applying every delta to a copy of the last snapshot
/// arrives at exactly the next one, and every snapshot holds together.
///
/// How long it takes is nextest's to judge — it prints `SLOW` past five
/// seconds and kills past sixty — rather than a wall-clock assertion inside a
/// case that shares the machine with the rest of the suite and would fail an
/// unrelated change on a loaded one.
///
/// # Panics
///
/// When a delta is refused, the two models differ, or a snapshot does not hold
/// together.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_deltas_rebuild_the_model() {
    let case = async {
        let mut generator = ModelGenerator::new(SEED);
        for round in 0..SEQUENCES {
            let mut registry = registry().expect("a registry");
            let mut deltas = registry.deltas();
            let mut held = registry.snapshot();
            for operation in generator.operations(OPERATIONS) {
                run(&mut registry, &operation).await;
                for numbered in drain(&mut deltas) {
                    apply(&mut held, numbered.generation, &numbered.value).unwrap_or_else(
                        |error| panic!("round {round}: {error} applying {:?}", numbered.value),
                    );
                }
                let snapshot = registry.snapshot();
                assert_eq!(held, snapshot, "round {round}: the deltas said otherwise");
                snapshot
                    .validate()
                    .unwrap_or_else(|error| panic!("round {round} is broken: {error}"));
            }
        }
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the convergence case finishes");
}

/// A pane's exit removes it with the status it ended by, its tab when it was
/// the last pane, and its session when that was the last tab — in that order.
///
/// # Panics
///
/// When the cascade does not happen, or happens in another order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_a_pane_exit_cascades() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let mut deltas = registry.deltas();
        let session = registry
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        let _started = drain(&mut deltas);
        let pane = panes(&registry.snapshot())
            .first()
            .map(|(_tab, pane)| *pane)
            .expect("the only pane");
        registry
            .pane(pane)
            .expect("the pane")
            .input(b"exit\n".to_vec())
            .expect("exit is written");

        let mut emitted = Vec::new();
        let arrived = {
            let mut collect = || {
                registry.ingest();
                emitted.extend(drain(&mut deltas));
                emitted.len() >= 3
            };
            let mut attempts: usize = 0;
            loop {
                if collect() {
                    break true;
                }
                attempts = attempts.saturating_add(1);
                if attempts > POLL_ATTEMPTS {
                    break false;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        };
        assert!(arrived, "the exit became deltas, saw {:?}", kinds(&emitted));
        assert_eq!(
            kinds(&emitted),
            ["PaneRemoved", "TabRemoved", "SessionRemoved"],
            "the cascade"
        );
        match emitted.first().map(|numbered| &numbered.value) {
            Some(Delta::PaneRemoved {
                reason: RemovalReason::Exited(_status),
                ..
            }) => {}
            other => panic!("a pane that exited was removed as {other:?}"),
        }
        assert_eq!(
            registry.snapshot().sessions.len(),
            0,
            "session {session:?} is gone"
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the cascade case finishes");
}

/// A pane placed before and after its target, in both directions, arranges the
/// tab as asked — and one placed into a split that already divides that way is
/// flattened into it rather than nested inside it.
///
/// # Panics
///
/// When an arrangement is not the one asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_placement_arranges_and_flattens() {
    let case = async {
        for (direction, before) in [
            (SplitDirection::Horizontal, false),
            (SplitDirection::Horizontal, true),
            (SplitDirection::Vertical, false),
            (SplitDirection::Vertical, true),
        ] {
            let mut registry = registry().expect("a registry");
            let _session = registry
                .create_session("work".to_owned(), COLUMNS, ROWS, None)
                .await
                .expect("a session");
            let model = registry.snapshot();
            let (tab, first) = panes(&model).first().copied().expect("the only pane");
            let placement = Placement {
                target: first,
                direction,
                before,
            };
            let second = registry
                .create_pane(tab, placement, COLUMNS, ROWS, None)
                .await
                .expect("a second pane");
            let arranged = layout_of(&registry.snapshot(), tab).expect("a layout");
            let expected = if before {
                vec![second, first]
            } else {
                vec![first, second]
            };
            assert_eq!(
                arranged.leaves(),
                expected,
                "{direction:?}, before {before}"
            );

            // A third pane split the same way as the tab already divides is
            // flattened into the one split, not nested inside it.
            let third = registry
                .create_pane(
                    tab,
                    Placement {
                        target: second,
                        direction,
                        before: false,
                    },
                    COLUMNS,
                    ROWS,
                    None,
                )
                .await
                .expect("a third pane");
            let flattened = layout_of(&registry.snapshot(), tab).expect("a layout");
            assert_eq!(flattened.depth(), 2, "{flattened:?} is nested, not flat");
            assert!(
                flattened.leaves().contains(&third),
                "the third pane is placed"
            );
            registry.snapshot().validate().expect("it holds together");
        }
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the placement case finishes");
}

/// A layout whose leaves are not the tab's panes is refused and changes
/// nothing; one that is valid but not canonical is stored canonical.
///
/// # Panics
///
/// When a wrong layout is stored, or a right one is stored as written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_a_layout_must_place_exactly_the_tabs_panes() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let _session = registry
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        let model = registry.snapshot();
        let (tab, first) = panes(&model).first().copied().expect("the only pane");
        let second = registry
            .create_pane(
                tab,
                Placement {
                    target: first,
                    direction: SplitDirection::Horizontal,
                    before: false,
                },
                COLUMNS,
                ROWS,
                None,
            )
            .await
            .expect("a second pane");
        let before = registry.snapshot();
        let mut deltas = registry.deltas();

        assert!(
            registry.set_layout(tab, LayoutNode::Leaf(first)).is_err(),
            "a layout that places only one of two panes was stored"
        );
        assert!(
            registry
                .set_layout(tab, LayoutNode::Leaf(PaneId(u64::MAX)))
                .is_err(),
            "a layout naming a pane the tab does not hold was stored"
        );
        assert_eq!(registry.snapshot(), before, "a refusal changed the model");
        assert!(drain(&mut deltas).is_empty(), "a refusal said something");

        // Weights that say the same thing at a different scale are one
        // arrangement, and the registry stores the canonical one.
        let unnormalized = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            children: vec![
                Weighted {
                    node: LayoutNode::Leaf(first),
                    weight: 50,
                },
                Weighted {
                    node: LayoutNode::Leaf(second),
                    weight: 50,
                },
            ],
        };
        registry
            .set_layout(tab, unnormalized)
            .expect("a valid layout is stored");
        let stored = layout_of(&registry.snapshot(), tab).expect("a layout");
        assert_eq!(
            stored,
            LayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    Weighted {
                        node: LayoutNode::Leaf(first),
                        weight: 1
                    },
                    Weighted {
                        node: LayoutNode::Leaf(second),
                        weight: 1
                    },
                ],
            },
            "the layout was stored as it was written"
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the layout case finishes");
}

/// Ids are minted once and never reused: a pane created, closed and created
/// again is a different pane, so nothing a client holds ever comes to mean
/// something else.
///
/// # Panics
///
/// When an id comes round again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_ids_are_never_reused() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let mut sessions = Vec::new();
        let mut tabs_seen = Vec::new();
        let mut panes_seen = Vec::new();
        for _round in 0..4 {
            let session = registry
                .create_session("work".to_owned(), COLUMNS, ROWS, None)
                .await
                .expect("a session");
            let model = registry.snapshot();
            sessions.push(session);
            tabs_seen.extend(tabs(&model).into_iter().map(|(_session, tab)| tab));
            panes_seen.extend(panes(&model).into_iter().map(|(_tab, pane)| pane));
            registry.close_session(session).expect("a close");
        }
        for (named, mut seen) in [
            (
                "sessions",
                sessions.iter().map(|held| held.0).collect::<Vec<u64>>(),
            ),
            ("tabs", tabs_seen.iter().map(|held| held.0).collect()),
            ("panes", panes_seen.iter().map(|held| held.0).collect()),
        ] {
            let before = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), before, "an id came round again among {named}");
        }
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the identity case finishes");
}

/// A receiver that falls further behind than the channel holds is told so, and
/// nothing in the registry is disturbed by it.
///
/// # Panics
///
/// When a lagging receiver is not told, or the registry notices.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_a_lagging_receiver_disturbs_nothing() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let session = registry
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        let mut behind = registry.deltas();
        let overrun = DELTA_BROADCAST_CAPACITY.saturating_add(DELTA_BROADCAST_CAPACITY / 10);
        for round in 0..overrun {
            registry
                .rename_session(session, format!("work-{round}"))
                .expect("a rename");
        }
        match behind.try_recv() {
            Err(broadcast::error::TryRecvError::Lagged(missed)) => {
                assert!(missed > 0, "a lagging receiver missed nothing");
            }
            other => panic!("a receiver {overrun} deltas behind was not told: {other:?}"),
        }
        let model = registry.snapshot();
        model.validate().expect("the registry still holds together");
        assert_eq!(
            model.sessions.first().map(|held| held.name.clone()),
            Some(format!("work-{}", overrun.saturating_sub(1))),
            "the last rename is the one that stands"
        );
        // A fresh receiver takes up from here, which is what a client that
        // asked for a snapshot after `Lagged` would do.
        let mut fresh = registry.deltas();
        registry
            .rename_session(session, "after".to_owned())
            .expect("a rename");
        assert_eq!(
            kinds(&drain(&mut fresh)),
            ["SessionRenamed"],
            "from here on"
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the lag case finishes");
}

/// What a pane says about itself becomes model state: a title and a working
/// directory from a real shell with the integration asset, and a size from a
/// resize.
///
/// This is the one case whose panes are not `sh`: a shell that reports nothing
/// has nothing to ingest.
///
/// # Panics
///
/// When what the shell said never reaches the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_what_a_pane_says_becomes_deltas() {
    let case = async {
        let asset = format!(
            "{}/../iznik-testkit/assets/shell-integration.bash",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut registry = registry_running(Program::Command {
            path: "bash".into(),
            arguments: vec!["--rcfile".to_owned(), asset, "-i".to_owned()],
        })
        .expect("a registry");
        let mut deltas = registry.deltas();
        let _session = registry
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        let _started = drain(&mut deltas);
        let pane = panes(&registry.snapshot())
            .first()
            .map(|(_tab, held)| *held)
            .expect("the only pane");

        registry
            .pane(pane)
            .expect("the pane")
            .input(b"printf '\\033]0;a title\\007'\n".to_vec())
            .expect("a title is asked for");
        registry
            .pane(pane)
            .expect("the pane")
            .resize(100, 40)
            .expect("a resize");

        let mut seen: Vec<String> = Vec::new();
        let mut attempts: usize = 0;
        loop {
            registry.ingest();
            seen.extend(kinds(&drain(&mut deltas)));
            let complete = ["PaneTitle", "PaneWorkingDirectory", "PaneResized"]
                .iter()
                .all(|wanted| seen.iter().any(|held| held == wanted));
            if complete {
                break;
            }
            attempts = attempts.saturating_add(1);
            assert!(attempts <= POLL_ATTEMPTS, "the shell said only {seen:?}");
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        let model = registry.snapshot();
        let held = model
            .sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .find(|held| held.id == pane)
            .expect("the pane in the model");
        assert_eq!(held.title, "a title", "the title reached the model");
        assert!(
            held.working_directory.is_some(),
            "the working directory reached the model"
        );
        assert_eq!((held.columns, held.rows), (100, 40), "the size did too");
        let _closed = registry.close_pane(pane);
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the ingestion case finishes");
}

/// How many panes the budget in the eviction case holds at once.
const BUDGETED_PANES: usize = 3;

/// How many panes that case opens and closes in front of the one that stays.
const SCRATCH_PANES: usize = 10;

/// A pane that is gone gives its share of the history budget back, so a pane
/// that stays is not shrunk to pay for panes that no longer exist.
///
/// Without that, a closed pane's entry keeps its bytes committed and, being
/// the least recently focused thing left, sits ahead of every live pane in the
/// order: it is the live pane's scrollback that is taken away.
///
/// # Panics
///
/// When the pane that stays loses history to panes that are gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_registry_a_closed_pane_gives_its_history_back() {
    let case = async {
        let budget = Arc::new(Mutex::new(HistoryBudget::new(
            DEFAULT_PANE_HISTORY_BYTES.saturating_mul(BUDGETED_PANES),
        )));
        let mut registry = registry_sharing(Arc::clone(&budget)).expect("a registry");
        let session = registry
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await
            .expect("a session");
        let (_tab, stays) = panes(&registry.snapshot())
            .first()
            .copied()
            .expect("a pane");
        let held = |shared: &Arc<Mutex<HistoryBudget>>| {
            shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .history(stays)
                .map_or(0, PaneHistory::capacity)
        };
        let at_first = held(&budget);
        assert_eq!(
            at_first, DEFAULT_PANE_HISTORY_BYTES,
            "the only pane has a whole ring"
        );
        for round in 0..SCRATCH_PANES {
            let tab = registry
                .create_tab(session, format!("scratch-{round}"), COLUMNS, ROWS, None)
                .await
                .expect("a scratch tab");
            registry.close_tab(tab).expect("it closes again");
            assert_eq!(
                held(&budget),
                at_first,
                "round {round} took history from the pane that stays"
            );
        }
        registry.close_session(session).expect("a close");
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the budget case finishes");
}
