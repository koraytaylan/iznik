//! The shared generator held to what its consumers rely on: a seed is the
//! whole of its randomness, every model it builds holds together, and every
//! sequence of changes it describes leads exactly where it says.

use iznik_protocol::identity::Generation;
use iznik_protocol::model::HostModel;
use iznik_protocol::reconcile::apply;
use iznik_testkit::generate::{ModelGenerator, RegistryOperation, chosen};

/// How many models and sequences the properties are checked over.
const ROUNDS: usize = 500;

/// How many changes a generated sequence carries.
const CHANGES: usize = 6;

/// How many operations a generated operation sequence carries.
const OPERATIONS: usize = 20;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_1102_5555;

/// A second seed, to show that the sequence follows the seed and not the call.
const OTHER_SEED: u64 = 0x2026_0828_1102_6666;

/// Everything a generator hands out for one seed, in order.
///
/// # Panics
///
/// Never; it only draws.
fn drawn(seed: u64) -> (Vec<HostModel>, Vec<Vec<RegistryOperation>>) {
    let mut generator = ModelGenerator::new(seed);
    let mut models = Vec::new();
    let mut operations = Vec::new();
    for _round in 0..ROUNDS {
        let start = generator.model();
        let sequence = generator.changes(&start, CHANGES);
        models.push(sequence.finish);
        operations.push(generator.operations(OPERATIONS));
    }
    (models, operations)
}

/// The same seed yields the same everything, and another seed does not.
///
/// # Panics
///
/// When a seed does not determine what is drawn.
#[test]
fn generate_a_seed_is_the_whole_of_the_randomness() {
    assert_eq!(drawn(SEED), drawn(SEED), "one seed, one sequence");
    assert_ne!(
        drawn(SEED),
        drawn(OTHER_SEED),
        "two seeds drew the same thing"
    );
}

/// Every model the generator builds holds together, and so does every model
/// its changes lead to.
///
/// # Panics
///
/// When a generated model does not validate.
#[test]
fn generate_every_model_it_builds_holds_together() {
    let mut generator = ModelGenerator::new(SEED);
    for round in 0..ROUNDS {
        let start = generator.model();
        start
            .validate()
            .unwrap_or_else(|error| panic!("round {round} started broken: {error}"));
        let sequence = generator.changes(&start, CHANGES);
        sequence
            .finish
            .validate()
            .unwrap_or_else(|error| panic!("round {round} finished broken: {error}"));
        assert_eq!(sequence.start, start, "the sequence names where it began");
    }
}

/// Every sequence of changes applies cleanly to the model it starts from and
/// arrives at exactly the model it names.
///
/// # Panics
///
/// When a delta is refused, or the deltas arrive somewhere else.
#[test]
fn generate_every_sequence_arrives_where_it_says() {
    let mut generator = ModelGenerator::new(SEED);
    for round in 0..ROUNDS {
        let start = generator.model();
        let sequence = generator.changes(&start, CHANGES);
        let mut held = sequence.start.clone();
        for delta in sequence.deltas() {
            let generation = Generation(held.generation.0.saturating_add(1));
            apply(&mut held, generation, &delta)
                .unwrap_or_else(|error| panic!("round {round}: {error} applying {delta:?}"));
        }
        assert_eq!(held, sequence.finish, "round {round} arrived elsewhere");
        assert_eq!(
            held.generation.0,
            sequence
                .start
                .generation
                .0
                .saturating_add(u64::try_from(sequence.deltas().len()).unwrap_or(0)),
            "round {round}: one delta, one generation"
        );
    }
}

/// A chooser names a position by wrapping, so an operation generated before a
/// model existed still names something the model holds.
///
/// # Panics
///
/// When a chooser does not wrap, or finds something in nothing.
#[test]
fn generate_a_chooser_wraps_onto_what_there_is() {
    let held = ["first", "second", "third"];
    assert_eq!(chosen(&held, 0), Some(&"first"), "the first");
    assert_eq!(chosen(&held, 2), Some(&"third"), "the last");
    assert_eq!(chosen(&held, 3), Some(&"first"), "round again");
    assert_eq!(chosen(&held, 4_000), Some(&"second"), "far round again");
    let empty: [&str; 0] = [];
    assert_eq!(chosen(&empty, 0), None, "nothing to choose from");
}

/// An operation sequence is as long as it was asked for and covers every kind
/// of operation a registry offers, so a test driving it exercises all of them.
///
/// # Panics
///
/// When a kind never comes up.
#[test]
fn generate_operations_cover_every_kind() {
    let mut generator = ModelGenerator::new(SEED);
    let operations = generator.operations(OPERATIONS.saturating_mul(ROUNDS));
    assert_eq!(
        operations.len(),
        OPERATIONS.saturating_mul(ROUNDS),
        "as many as asked for"
    );
    let mut kinds: Vec<&str> = operations
        .iter()
        .map(|operation| match operation {
            RegistryOperation::CreateSession { .. } => "CreateSession",
            RegistryOperation::CreateTab { .. } => "CreateTab",
            RegistryOperation::CreatePane { .. } => "CreatePane",
            RegistryOperation::ClosePane { .. } => "ClosePane",
            RegistryOperation::MovePane { .. } => "MovePane",
            RegistryOperation::RenameSession { .. } => "RenameSession",
            RegistryOperation::RenameTab { .. } => "RenameTab",
            RegistryOperation::CloseTab { .. } => "CloseTab",
            RegistryOperation::CloseSession { .. } => "CloseSession",
            RegistryOperation::ReorderTabs { .. } => "ReorderTabs",
            RegistryOperation::SetLayout { .. } => "SetLayout",
        })
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), 11, "the kinds drawn were {kinds:?}");
}

/// Every kind of change comes up over enough rounds, so the convergence
/// property is not proving one delta fourteen times.
///
/// # Panics
///
/// When a delta variant never appears.
#[test]
fn generate_changes_cover_every_delta() {
    let kinds = variants_drawn(SEED);
    assert_eq!(kinds.len(), 14, "the deltas drawn were {kinds:?}");
}

/// A seed of zero draws like any other. Zero is a fixed point of the xorshift,
/// so a generator that started from it unchanged would draw nothing but zeros
/// for ever: one session, one kind of change, one name — and every property
/// checked over it would pass while proving almost nothing.
///
/// # Panics
///
/// When a seed of zero does not draw.
#[test]
fn generate_a_seed_of_zero_still_draws() {
    let kinds = variants_drawn(0);
    assert_eq!(kinds.len(), 14, "a seed of zero drew {kinds:?}");
}

/// The names of the delta variants a seed draws over `ROUNDS` rounds.
///
/// # Panics
///
/// Never; it only draws.
fn variants_drawn(seed: u64) -> Vec<String> {
    let mut generator = ModelGenerator::new(seed);
    let mut kinds: Vec<String> = Vec::new();
    for _round in 0..ROUNDS {
        let start = generator.model();
        for delta in generator.changes(&start, CHANGES).deltas() {
            let named = format!("{delta:?}");
            let kind = named
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned();
            kinds.push(kind);
        }
    }
    kinds.sort();
    kinds.dedup();
    kinds
}
