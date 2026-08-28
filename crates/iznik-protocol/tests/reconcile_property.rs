//! The reconciler against the property the whole client design rests on: a
//! snapshot followed by its deltas is the next snapshot. It is proven by
//! generation rather than by example, against models the generator built
//! directly, so the deltas are checked against something nobody produced by
//! applying them.
//!
//! Beside it, the discipline that makes the property usable: a delta out of
//! order is refused and changes nothing, a delta that would break an invariant
//! is refused by name and changes nothing, and a layout is stored canonical
//! however a peer wrote it.

use std::error::Error;

use iznik_protocol::delta::{Delta, RemovalReason};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{
    HostModel, LayoutNode, ModelError, Pane, Session, SplitDirection, Tab, Weighted,
};
use iznik_protocol::reconcile::{ReconcileError, apply};
use iznik_testkit::generate::ModelGenerator;

/// How many sequences the convergence property is checked over.
const SEQUENCES: usize = 10_000;

/// How many changes a generated sequence carries.
const CHANGES: usize = 5;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_1102_0003;

/// Why a case could not be built.
type Failure = Box<dyn Error>;

/// A leaf.
fn leaf(pane: u64) -> LayoutNode {
    LayoutNode::Leaf(PaneId(pane))
}

/// A split side by side, of the given children and weights.
fn horizontal(children: Vec<(LayoutNode, u32)>) -> LayoutNode {
    LayoutNode::Split {
        direction: SplitDirection::Horizontal,
        children: children
            .into_iter()
            .map(|(node, weight)| Weighted { node, weight })
            .collect(),
    }
}

/// A pane with a size and no directory.
fn pane(id: u64) -> Pane {
    Pane {
        id: PaneId(id),
        title: "sh".to_owned(),
        working_directory: None,
        columns: 80,
        rows: 24,
    }
}

/// A host at generation one: one session, one tab, two panes side by side.
fn two_pane_host() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "session".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "tab".to_owned(),
                panes: vec![pane(1), pane(2)],
                layout: horizontal(vec![(leaf(1), 1), (leaf(2), 1)]),
            }],
        }],
    }
}

/// Applies a delta as the model's next, and says what happened.
///
/// # Errors
///
/// Whatever the reconciler refused.
fn next(model: &mut HostModel, delta: &Delta) -> Result<(), ReconcileError> {
    let generation = Generation(model.generation.0.saturating_add(1));
    apply(model, generation, delta)
}

/// A snapshot followed by its deltas is the next snapshot: over ten thousand
/// generated sequences the model is whole after every change and, at the end,
/// exactly the model the generator built directly.
///
/// # Panics
///
/// When a generated delta is refused, a model between changes does not hold
/// together, or the two models differ.
#[test]
fn reconcile_property_a_snapshot_and_its_deltas_converge() {
    let mut generator = ModelGenerator::new(SEED);
    let mut applied = 0_usize;
    for index in 0..SEQUENCES {
        let start = generator.model();
        start
            .validate()
            .unwrap_or_else(|error| panic!("sequence {index} starts broken: {error}"));
        let sequence = generator.changes(&start, CHANGES);
        let mut held = sequence.start.clone();
        for change in &sequence.changes {
            for delta in change {
                next(&mut held, delta)
                    .unwrap_or_else(|error| panic!("sequence {index}: {error} applying {delta:?}"));
                applied = applied.saturating_add(1);
            }
            held.validate().unwrap_or_else(|error| {
                panic!("sequence {index} is broken between changes: {error}")
            });
        }
        assert_eq!(
            held, sequence.finish,
            "sequence {index}: the deltas did not arrive at the model they describe"
        );
    }
    assert!(
        applied > SEQUENCES,
        "only {applied} deltas over {SEQUENCES} sequences"
    );
}

/// A delta numbered anything but the model's next is refused, and the model is
/// exactly what it was; the right number advances the generation by one.
///
/// # Panics
///
/// When a gap is accepted, a refusal changes the model, or the generation does
/// not advance by exactly one.
#[test]
fn reconcile_property_a_generation_gap_is_refused_and_changes_nothing() {
    let before = two_pane_host();
    let delta = Delta::SessionRenamed {
        session: SessionId(1),
        name: "renamed".to_owned(),
    };
    for offered in [0, 1, 3, 99] {
        let mut held = before.clone();
        assert_eq!(
            apply(&mut held, Generation(offered), &delta),
            Err(ReconcileError::GenerationGap {
                expected: Generation(2),
                received: Generation(offered)
            }),
            "generation {offered} is not the model's next"
        );
        assert_eq!(held, before, "a gap left a mark");
    }
    let mut held = before.clone();
    assert_eq!(apply(&mut held, Generation(2), &delta), Ok(()));
    assert_eq!(held.generation, Generation(2), "one delta, one generation");
}

/// Every refusal names the identity involved and leaves the model exactly as
/// it was.
///
/// # Panics
///
/// When a broken delta is accepted, refused by another variant, or changes the
/// model.
#[test]
fn reconcile_property_a_refusal_leaves_no_trace() {
    let before = two_pane_host();
    for (delta, expected) in refused_deltas() {
        let mut held = before.clone();
        assert_eq!(next(&mut held, &delta), Err(expected.clone()), "{delta:?}");
        assert_eq!(held, before, "{expected} left a mark");
    }
}

/// Every delta `two_pane_host` refuses, with the refusal it must give.
fn refused_deltas() -> Vec<(Delta, ReconcileError)> {
    let mut refused = refused_for_naming_nothing();
    refused.extend(refused_for_what_is_carried());
    refused
}

/// The deltas refused for naming an identity the host does not hold.
fn refused_for_naming_nothing() -> Vec<(Delta, ReconcileError)> {
    vec![
        (
            Delta::SessionRenamed {
                session: SessionId(9),
                name: "away".to_owned(),
            },
            ReconcileError::UnknownSession {
                session: SessionId(9),
            },
        ),
        (
            Delta::TabRenamed {
                tab: TabId(9),
                name: "away".to_owned(),
            },
            ReconcileError::UnknownTab { tab: TabId(9) },
        ),
        (
            Delta::PaneTitle {
                pane: PaneId(9),
                title: "away".to_owned(),
            },
            ReconcileError::UnknownPane { pane: PaneId(9) },
        ),
        (
            Delta::PaneAdded {
                tab: TabId(9),
                pane: pane(3),
            },
            ReconcileError::UnknownTab { tab: TabId(9) },
        ),
        (
            Delta::PaneRemoved {
                pane: PaneId(9),
                reason: RemovalReason::Closed,
            },
            ReconcileError::UnknownPane { pane: PaneId(9) },
        ),
        (
            Delta::PaneMoved {
                pane: PaneId(1),
                to_tab: TabId(9),
            },
            ReconcileError::UnknownTab { tab: TabId(9) },
        ),
    ]
}

/// The deltas refused for what they carry rather than what they name.
fn refused_for_what_is_carried() -> Vec<(Delta, ReconcileError)> {
    vec![
        (
            Delta::SessionRenamed {
                session: SessionId(1),
                name: String::new(),
            },
            ReconcileError::Invalid {
                error: ModelError::EmptySessionName {
                    session: SessionId(1),
                },
            },
        ),
        (
            Delta::TabRenamed {
                tab: TabId(1),
                name: String::new(),
            },
            ReconcileError::Invalid {
                error: ModelError::EmptyTabName { tab: TabId(1) },
            },
        ),
        (
            Delta::PaneAdded {
                tab: TabId(1),
                pane: pane(2),
            },
            ReconcileError::Invalid {
                error: ModelError::DuplicatePane { pane: PaneId(2) },
            },
        ),
        (
            Delta::LayoutChanged {
                tab: TabId(1),
                layout: leaf(1),
            },
            ReconcileError::Invalid {
                error: ModelError::PaneMissingFromLayout {
                    tab: TabId(1),
                    pane: PaneId(2),
                },
            },
        ),
        (
            Delta::TabsReordered {
                session: SessionId(1),
                order: vec![TabId(1), TabId(1)],
            },
            ReconcileError::NotAPermutation {
                session: SessionId(1),
            },
        ),
        (
            Delta::TabsReordered {
                session: SessionId(1),
                order: Vec::new(),
            },
            ReconcileError::NotAPermutation {
                session: SessionId(1),
            },
        ),
    ]
}

/// A layout a peer wrote uncanonically is stored canonical, so two models that
/// hold the same arrangement compare equal.
///
/// # Errors
///
/// Never; the signature is the test's.
///
/// # Panics
///
/// When the stored layout is not the normalized one.
#[test]
fn reconcile_property_a_layout_is_normalized_on_application() -> Result<(), Failure> {
    let mut held = two_pane_host();
    let as_written = horizontal(vec![
        (horizontal(vec![(leaf(1), 50), (leaf(2), 50)]), 1),
        (horizontal(Vec::new()), 1),
    ]);
    next(
        &mut held,
        &Delta::LayoutChanged {
            tab: TabId(1),
            layout: as_written,
        },
    )?;
    let stored = held
        .sessions
        .first()
        .and_then(|session| session.tabs.first())
        .map(|tab| tab.layout.clone())
        .ok_or("the only tab")?;
    assert_eq!(
        stored,
        horizontal(vec![(leaf(1), 1), (leaf(2), 1)]),
        "the layout was stored as it was written"
    );
    held.validate()?;
    Ok(())
}
