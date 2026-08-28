//! The host model against its golden and its invariants: every line encodes,
//! decodes and validates as it says it does, every broken invariant is refused
//! by the variant naming the identity that breaks it, normalization flattens,
//! scales, collapses and settles, the layout operations keep every other pane
//! where it was, identity survives a removal, and nesting is bounded on both
//! sides of the codec.

use std::error::Error;
use std::path::Path;

use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::MessageError;
use iznik_protocol::model::{
    HostModel, LayoutNode, MAXIMUM_LAYOUT_DEPTH, ModelError, Pane, Session, SplitDirection, Tab,
    Weighted, decode_host_model, encode_host_model,
};
use iznik_testkit::golden;
use serde_json::Value;

/// The golden fixture, relative to this crate.
const FIXTURE: &str = "tests/fixtures/model.jsonl";

/// How many trees the properties over generated trees are checked against.
const GENERATED_TREES: usize = 1_000;

/// The seed the tree generator starts from, so a failure is reproducible.
const GENERATOR_SEED: u64 = 0x2026_0828_1101_0003;

/// The deepest tree the generator builds: past anything normalization has to
/// reach, far inside [`MAXIMUM_LAYOUT_DEPTH`].
const GENERATED_DEPTH: usize = 6;

/// The widest a generated split is and the largest weight it hands out: small
/// numbers, so a scaled weight is legible and zero is drawn often.
const GENERATED_SPREAD: u64 = 4;

/// Why a line could not be read, or a case could not be built.
type Failure = Box<dyn Error>;

/// A field of a JSON object.
///
/// # Errors
///
/// When the field is absent.
fn field<'value>(object: &'value Value, name: &str) -> Result<&'value Value, Failure> {
    object
        .get(name)
        .ok_or_else(|| format!("field `{name}` is missing").into())
}

/// An unsigned integer field.
///
/// # Errors
///
/// When the field is absent or not an unsigned integer.
fn integer_field(object: &Value, name: &str) -> Result<u64, Failure> {
    field(object, name)?
        .as_u64()
        .ok_or_else(|| format!("field `{name}` is not an unsigned integer").into())
}

/// An unsigned integer field narrowed to the width the model uses.
///
/// # Errors
///
/// When the field is absent, not an unsigned integer, or too wide.
fn narrow_field<Number: TryFrom<u64>>(object: &Value, name: &str) -> Result<Number, Failure> {
    Number::try_from(integer_field(object, name)?)
        .map_err(|_error| format!("field `{name}` does not fit its width").into())
}

/// A string field.
///
/// # Errors
///
/// When the field is absent or not a string.
fn string_field(object: &Value, name: &str) -> Result<String, Failure> {
    field(object, name)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("field `{name}` is not a string").into())
}

/// A field holding a JSON array.
///
/// # Errors
///
/// When the field is absent or not an array.
fn array_field<'value>(object: &'value Value, name: &str) -> Result<&'value [Value], Failure> {
    field(object, name)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("field `{name}` is not an array").into())
}

/// A variant: a bare name for a unit variant, or `{"Name": fields}`.
///
/// # Errors
///
/// When the value is neither.
fn variant(value: &Value) -> Result<(String, Value), Failure> {
    if let Some(name) = value.as_str() {
        return Ok((name.to_owned(), Value::Null));
    }
    let object = value
        .as_object()
        .ok_or("a variant is a string or a one-key object")?;
    let mut entries = object.iter();
    match (entries.next(), entries.next()) {
        (Some((name, fields)), None) => Ok((name.clone(), fields.clone())),
        _other => Err("a variant object has exactly one key".into()),
    }
}

/// The layout node a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn layout_of(value: &Value) -> Result<LayoutNode, Failure> {
    let (name, fields) = variant(value)?;
    match name.as_str() {
        "Leaf" => Ok(LayoutNode::Leaf(PaneId(
            fields.as_u64().ok_or("a leaf names a pane id")?,
        ))),
        "Split" => {
            let direction = match string_field(&fields, "direction")?.as_str() {
                "Horizontal" => SplitDirection::Horizontal,
                "Vertical" => SplitDirection::Vertical,
                other => return Err(format!("no SplitDirection `{other}`").into()),
            };
            let mut children = Vec::new();
            for child in array_field(&fields, "children")? {
                children.push(Weighted {
                    node: layout_of(field(child, "node")?)?,
                    weight: narrow_field(child, "weight")?,
                });
            }
            Ok(LayoutNode::Split {
                direction,
                children,
            })
        }
        other => Err(format!("no LayoutNode variant `{other}`").into()),
    }
}

/// The pane a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn pane_of(value: &Value) -> Result<Pane, Failure> {
    let working_directory = match field(value, "working_directory")? {
        Value::Null => None,
        other => Some(
            other
                .as_str()
                .ok_or("a directory is a string or null")?
                .to_owned(),
        ),
    };
    Ok(Pane {
        id: PaneId(integer_field(value, "id")?),
        title: string_field(value, "title")?,
        working_directory,
        columns: narrow_field(value, "columns")?,
        rows: narrow_field(value, "rows")?,
    })
}

/// The tab a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn tab_of(value: &Value) -> Result<Tab, Failure> {
    let mut panes = Vec::new();
    for pane in array_field(value, "panes")? {
        panes.push(pane_of(pane)?);
    }
    Ok(Tab {
        id: TabId(integer_field(value, "id")?),
        name: string_field(value, "name")?,
        panes,
        layout: layout_of(field(value, "layout")?)?,
    })
}

/// The host model a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn model_of(value: &Value) -> Result<HostModel, Failure> {
    let mut sessions = Vec::new();
    for session in array_field(value, "sessions")? {
        let mut tabs = Vec::new();
        for tab in array_field(session, "tabs")? {
            tabs.push(tab_of(tab)?);
        }
        sessions.push(Session {
            id: SessionId(integer_field(session, "id")?),
            name: string_field(session, "name")?,
            tabs,
        });
    }
    Ok(HostModel {
        generation: Generation(integer_field(value, "generation")?),
        sessions,
    })
}

/// The refusal a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn message_error_of(value: &Value) -> Result<MessageError, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "UnknownDiscriminant" => MessageError::UnknownDiscriminant {
            channel: narrow_field(&fields, "channel")?,
            discriminant: narrow_field(&fields, "discriminant")?,
        },
        "Truncated" => MessageError::Truncated {
            discriminant: narrow_field(&fields, "discriminant")?,
            needed: narrow_field(&fields, "needed")?,
            available: narrow_field(&fields, "available")?,
        },
        "TrailingBytes" => MessageError::TrailingBytes {
            discriminant: narrow_field(&fields, "discriminant")?,
            count: narrow_field(&fields, "count")?,
        },
        "Utf8" => MessageError::Utf8 {
            discriminant: narrow_field(&fields, "discriminant")?,
        },
        "LayoutTooDeep" => MessageError::LayoutTooDeep {
            limit: narrow_field(&fields, "limit")?,
        },
        other => return Err(format!("no MessageError `{other}`").into()),
    })
}

/// The fixture's lines, in order.
///
/// # Errors
///
/// When the fixture cannot be loaded.
fn lines() -> Result<Vec<Value>, Failure> {
    Ok(golden::lines(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE),
    )?)
}

/// A leaf.
fn leaf(pane: u64) -> LayoutNode {
    LayoutNode::Leaf(PaneId(pane))
}

/// A split of the given children and their weights.
fn split(direction: SplitDirection, children: Vec<(LayoutNode, u32)>) -> LayoutNode {
    LayoutNode::Split {
        direction,
        children: children
            .into_iter()
            .map(|(node, weight)| Weighted { node, weight })
            .collect(),
    }
}

/// A pane with a title and a size, at the given id.
fn pane(id: u64) -> Pane {
    Pane {
        id: PaneId(id),
        title: "sh".to_owned(),
        working_directory: None,
        columns: 80,
        rows: 24,
    }
}

/// A tab holding the given panes, arranged the given way.
fn tab(id: u64, panes: &[u64], layout: LayoutNode) -> Tab {
    Tab {
        id: TabId(id),
        name: "tab".to_owned(),
        panes: panes.iter().map(|pane_id| pane(*pane_id)).collect(),
        layout,
    }
}

/// A tab holding one pane, its layout that pane's leaf.
fn simple_tab(id: u64, pane_id: u64) -> Tab {
    tab(id, &[pane_id], leaf(pane_id))
}

/// A session holding the given tabs.
fn session(id: u64, tabs: Vec<Tab>) -> Session {
    Session {
        id: SessionId(id),
        name: "session".to_owned(),
        tabs,
    }
}

/// A host at generation one holding the given sessions.
fn host(sessions: Vec<Session>) -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions,
    }
}

/// A host holding one session holding the given tab.
fn one_tab(only: Tab) -> HostModel {
    host(vec![session(1, vec![only])])
}

/// A split side by side.
fn horizontal(children: Vec<(LayoutNode, u32)>) -> LayoutNode {
    split(SplitDirection::Horizontal, children)
}

/// A split stacked.
fn vertical(children: Vec<(LayoutNode, u32)>) -> LayoutNode {
    split(SplitDirection::Vertical, children)
}

/// A deterministic source of arbitrary layout trees, including every shape
/// normalization exists to remove: splits nested in their own direction,
/// splits of one child, splits of none, and weights of zero. This is not
/// `iznik_testkit::generate`, which lands with the reconciler and produces
/// models that are already valid; what normalization is held to is the trees
/// that are not.
#[derive(Debug)]
struct TreeGenerator {
    /// The xorshift state.
    state: u64,
    /// The next pane id to hand out, so every leaf of a tree names its own.
    next_pane: u64,
}

impl TreeGenerator {
    /// A generator from a seed, whose first pane is one.
    fn new(seed: u64) -> TreeGenerator {
        TreeGenerator {
            state: seed,
            next_pane: 1,
        }
    }

    /// The next value of the xorshift.
    fn next(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state.wrapping_shl(13);
        state ^= state.wrapping_shr(7);
        state ^= state.wrapping_shl(17);
        self.state = state;
        state
    }

    /// The next value below `limit`.
    fn below(&mut self, limit: u64) -> u64 {
        self.next().checked_rem(limit).unwrap_or(0)
    }

    /// A pane id no tree of this generator has used.
    fn pane(&mut self) -> PaneId {
        let id = self.next_pane;
        self.next_pane = self.next_pane.saturating_add(1);
        PaneId(id)
    }

    /// A tree nesting at most `depth` levels.
    fn tree(&mut self, depth: usize) -> LayoutNode {
        if depth == 0 || self.below(GENERATED_SPREAD) == 0 {
            return LayoutNode::Leaf(self.pane());
        }
        let direction = if self.below(2) == 0 {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        };
        let count = self.below(GENERATED_SPREAD);
        let mut children = Vec::new();
        for _index in 0..count {
            let node = self.tree(depth.saturating_sub(1));
            let weight = u32::try_from(self.below(GENERATED_SPREAD)).unwrap_or(0);
            children.push(Weighted { node, weight });
        }
        LayoutNode::Split {
            direction,
            children,
        }
    }
}

/// Whether a tree is the shape [`LayoutNode::normalize`] produces: no split
/// nested in one of its own direction, and no split of a single child. A split
/// of none survives only at the root, with nothing to drop it into.
fn is_canonical(node: &LayoutNode, root: bool) -> bool {
    let LayoutNode::Split {
        direction,
        children,
    } = node
    else {
        return true;
    };
    if children.len() == 1 || (children.is_empty() && !root) {
        return false;
    }
    children.iter().all(|child| {
        !matches!(&child.node, LayoutNode::Split { direction: inner, .. } if inner == direction)
            && is_canonical(&child.node, false)
    })
}

/// A layout of `levels` splits nested one inside the next, each holding a
/// single child, around one leaf: the cheapest deep tree a peer can send, and
/// so the one the decoder's bound has to stop.
fn nested_layout_bytes(levels: usize) -> Vec<u8> {
    /// A `Leaf` naming pane one: the innermost node, and the only one.
    const LEAF: [u8; 9] = [1, 1, 0, 0, 0, 0, 0, 0, 0];
    /// A `Split`, `Horizontal`.
    const SPLIT: [u8; 2] = [0, 0];
    /// The one child each split holds, and the weight it holds it at.
    const ONE: u32 = 1;

    let mut bytes = LEAF.to_vec();
    for _index in 0..levels {
        let mut wrapped = SPLIT.to_vec();
        wrapped.extend_from_slice(&ONE.to_le_bytes());
        wrapped.append(&mut bytes);
        wrapped.extend_from_slice(&ONE.to_le_bytes());
        bytes = wrapped;
    }
    bytes
}

/// A one-session, one-tab, one-pane payload whose layout is the given bytes:
/// everything before it is what the encoder writes, so the tree is the only
/// thing under test.
///
/// # Errors
///
/// When the model the prefix is taken from does not encode.
fn payload_with_layout(layout: &[u8]) -> Result<Vec<u8>, Failure> {
    /// A leaf's bytes: its tag and the pane it names.
    const LEAF_LENGTH: usize = size_of::<u8>() + size_of::<u64>();

    let base = encode_host_model(&host(vec![session(1, vec![simple_tab(1, 1)])]))?;
    let cut = base
        .len()
        .checked_sub(LEAF_LENGTH)
        .ok_or("a leaf's bytes")?;
    let mut bytes = base
        .get(..cut)
        .ok_or("the bytes before the layout")?
        .to_vec();
    bytes.extend_from_slice(layout);
    Ok(bytes)
}

/// Holds one round-tripping line to the codec and to `validate`.
///
/// # Errors
///
/// When the line is malformed.
///
/// # Panics
///
/// When the codec or the validator does not behave as the line says.
fn check_model(description: &str, line: &Value) -> Result<(), Failure> {
    let model = model_of(field(line, "model")?)?;
    let wire = golden::bytes(&string_field(line, "hex")?)?;
    let encoded = encode_host_model(&model).map_err(|error| format!("{description}: {error}"))?;
    assert_eq!(
        golden::hex(&encoded),
        golden::hex(&wire),
        "{description}: encode"
    );
    assert_eq!(decode_host_model(&wire), Ok(model.clone()), "{description}");
    assert_eq!(
        model.validate().is_ok(),
        field(line, "valid")?
            .as_bool()
            .ok_or("`valid` is a boolean")?,
        "{description}: validate said {:?}",
        model.validate()
    );
    Ok(())
}

/// Every fixture line is the contract: the models in both directions and
/// under the validator, the refusals exactly as named.
///
/// # Panics
///
/// When a line is malformed, or the codec does not behave as it says.
#[test]
fn model_invariants_every_fixture_line_holds_in_both_directions() {
    let lines = lines().expect("the fixture loads");
    assert!(lines.len() > 10, "the fixture has {} lines", lines.len());
    for line in &lines {
        let description = string_field(line, "description").expect("a description");
        let outcome = match line.get("error") {
            Some(error) => (|| {
                let wire = golden::bytes(&string_field(line, "hex")?)?;
                assert_eq!(
                    decode_host_model(&wire),
                    Err(message_error_of(error)?),
                    "{description}"
                );
                Ok(())
            })(),
            None => check_model(&description, line),
        };
        outcome.unwrap_or_else(|error| panic!("{description}: {error}"));
    }
}

/// Every refusal `decode_host_model` documents is provoked: four by a fixture
/// line, and the fifth by a tree nested past the bound.
///
/// # Panics
///
/// When a refusal has nothing that provokes it.
#[test]
fn model_invariants_every_refusal_the_decoder_gives_is_provoked() {
    let lines = lines().expect("the fixture loads");
    let mut provoked: Vec<String> = lines
        .iter()
        .filter_map(|line| line.get("error"))
        .map(|error| variant(error).expect("an error variant").0)
        .collect();
    let deep = payload_with_layout(&nested_layout_bytes(MAXIMUM_LAYOUT_DEPTH + 1))
        .expect("a deep payload");
    match decode_host_model(&deep) {
        Err(MessageError::LayoutTooDeep { limit }) => {
            assert_eq!(limit, MAXIMUM_LAYOUT_DEPTH, "the bound it names");
            provoked.push("LayoutTooDeep".to_owned());
        }
        other => panic!("a tree past the bound was not refused: {other:?}"),
    }
    provoked.sort();
    provoked.dedup();
    assert_eq!(
        provoked,
        [
            "LayoutTooDeep",
            "TrailingBytes",
            "Truncated",
            "UnknownDiscriminant",
            "Utf8",
        ],
        "the refusals provoked"
    );
}

/// Every invariant has a model that breaks it, refused by the variant that
/// names the identity breaking it.
///
/// # Panics
///
/// When a broken model is accepted, or refused by another variant.
#[test]
fn model_invariants_validate_names_the_identity_of_every_broken_invariant() {
    let unnamed_session = Session {
        name: String::new(),
        ..session(1, vec![simple_tab(1, 1)])
    };
    let unnamed_tab = Tab {
        name: String::new(),
        ..simple_tab(1, 1)
    };
    let twice_named = host(vec![
        session(1, vec![simple_tab(1, 1)]),
        session(1, vec![simple_tab(2, 2)]),
    ]);
    let broken = vec![
        (
            twice_named,
            ModelError::DuplicateSession {
                session: SessionId(1),
            },
        ),
        (
            host(vec![session(1, vec![simple_tab(1, 1), simple_tab(1, 2)])]),
            ModelError::DuplicateTab { tab: TabId(1) },
        ),
        (
            host(vec![session(1, vec![simple_tab(1, 1), simple_tab(2, 1)])]),
            ModelError::DuplicatePane { pane: PaneId(1) },
        ),
        (
            host(vec![session(1, vec![])]),
            ModelError::SessionWithoutTabs {
                session: SessionId(1),
            },
        ),
        (
            one_tab(tab(1, &[], leaf(1))),
            ModelError::TabWithoutPanes { tab: TabId(1) },
        ),
        (
            one_tab(tab(1, &[1], leaf(2))),
            ModelError::LayoutNamesUnknownPane {
                tab: TabId(1),
                pane: PaneId(2),
            },
        ),
        (
            one_tab(tab(
                1,
                &[1, 2],
                horizontal(vec![(leaf(1), 1), (leaf(1), 1)]),
            )),
            ModelError::LayoutRepeatsPane {
                tab: TabId(1),
                pane: PaneId(1),
            },
        ),
        (
            one_tab(tab(1, &[1, 2], leaf(1))),
            ModelError::PaneMissingFromLayout {
                tab: TabId(1),
                pane: PaneId(2),
            },
        ),
        (
            one_tab(tab(
                1,
                &[1, 2],
                horizontal(vec![(leaf(1), 0), (leaf(2), 1)]),
            )),
            ModelError::ZeroWeight { tab: TabId(1) },
        ),
        (
            one_tab(tab(
                1,
                &[1, 2, 3],
                horizontal(vec![
                    (leaf(1), 1),
                    (horizontal(vec![(leaf(2), 1), (leaf(3), 1)]), 1),
                ]),
            )),
            ModelError::UnnormalizedLayout { tab: TabId(1) },
        ),
        (
            host(vec![unnamed_session]),
            ModelError::EmptySessionName {
                session: SessionId(1),
            },
        ),
        (
            one_tab(unnamed_tab),
            ModelError::EmptyTabName { tab: TabId(1) },
        ),
    ];
    for (model, expected) in broken {
        assert_eq!(model.validate(), Err(expected.clone()), "{expected}");
    }
    assert_eq!(
        one_tab(simple_tab(1, 1)).validate(),
        Ok(()),
        "the model every broken one varies"
    );
}

/// A canonical tree of `depth` levels: splits alternating direction, each
/// holding a leaf and the next split, so nothing flattens and nothing
/// collapses. Its panes are numbered from one, `depth` of them.
fn alternating_layout(depth: usize) -> LayoutNode {
    let deepest = u64::try_from(depth).unwrap_or(u64::MAX);
    let mut node = LayoutNode::Leaf(PaneId(deepest));
    let mut direction = SplitDirection::Horizontal;
    for level in (1..depth).rev() {
        let pane_id = u64::try_from(level).unwrap_or(u64::MAX);
        node = split(direction, vec![(leaf(pane_id), 1), (node, 1)]);
        direction = match direction {
            SplitDirection::Horizontal => SplitDirection::Vertical,
            SplitDirection::Vertical => SplitDirection::Horizontal,
        };
    }
    node
}

/// Normalization flattens a same-direction nesting with its weights scaled so
/// no pane's share changes, collapses a split of one child, drops a split of
/// none, and leaves a canonical tree alone.
///
/// # Panics
///
/// When any of those produces another tree.
#[test]
fn model_invariants_normalization_flattens_scales_collapses_and_drops() {
    // A child of weight 2 whose own children weigh 3 and 1 gives them 6 and 2
    // of the 12 the parent now divides, and the sibling of weight 1 takes the
    // remaining 4: exactly the half, the sixth and the third they all were.
    let nested = horizontal(vec![
        (horizontal(vec![(leaf(1), 3), (leaf(2), 1)]), 2),
        (leaf(3), 1),
    ]);
    assert_eq!(
        nested.normalize(),
        horizontal(vec![(leaf(1), 6), (leaf(2), 2), (leaf(3), 4)]),
        "a same-direction nesting is flattened with its weights scaled"
    );

    // Two lifted children: the halves become quarters and sixths of twelve.
    let two = horizontal(vec![
        (horizontal(vec![(leaf(1), 1), (leaf(2), 1)]), 1),
        (
            horizontal(vec![(leaf(3), 1), (leaf(4), 1), (leaf(5), 1)]),
            1,
        ),
    ]);
    assert_eq!(
        two.normalize(),
        horizontal(vec![
            (leaf(1), 3),
            (leaf(2), 3),
            (leaf(3), 2),
            (leaf(4), 2),
            (leaf(5), 2),
        ]),
        "every lifted child keeps the share it had"
    );

    assert_eq!(
        horizontal(vec![(leaf(1), 5)]).normalize(),
        leaf(1),
        "a split of one child is that child"
    );
    assert_eq!(
        horizontal(vec![(leaf(1), 1), (vertical(Vec::new()), 7), (leaf(2), 1)]).normalize(),
        horizontal(vec![(leaf(1), 1), (leaf(2), 1)]),
        "a split of no children is dropped"
    );
    let crossed = horizontal(vec![
        (leaf(1), 3),
        (vertical(vec![(leaf(2), 1), (leaf(3), 2)]), 5),
        (leaf(4), 1),
    ]);
    assert_eq!(
        crossed.clone().normalize(),
        crossed,
        "a split of the other direction stays, and a canonical tree is unchanged"
    );
    assert_eq!(leaf(9).normalize(), leaf(9), "a leaf is already canonical");
}

/// Over a thousand generated trees — same-direction nestings, splits of one
/// child, splits of none and weights of zero among them — normalization
/// settles in one pass, is canonical, and moves no pane.
///
/// # Panics
///
/// When a second pass changes a normalized tree, the shape is not canonical,
/// or the panes come out in another order.
#[test]
fn model_invariants_normalization_is_idempotent_and_canonical() {
    let mut generator = TreeGenerator::new(GENERATOR_SEED);
    let mut splits_flattened = 0_usize;
    for index in 0..GENERATED_TREES {
        let tree = generator.tree(GENERATED_DEPTH);
        let once = tree.clone().normalize();
        assert_eq!(
            once.clone().normalize(),
            once,
            "tree {index} is not a fixed point of normalization"
        );
        assert!(
            is_canonical(&once, true),
            "tree {index} normalized to {once:?}"
        );
        assert_eq!(
            once.leaves(),
            tree.leaves(),
            "tree {index} lost or moved a pane"
        );
        if !is_canonical(&tree, true) {
            splits_flattened = splits_flattened.saturating_add(1);
        }
    }
    assert!(
        splits_flattened > GENERATED_TREES / 10,
        "only {splits_flattened} of {GENERATED_TREES} generated trees needed normalizing"
    );
}

/// `replace_leaf` and `remove_leaf` put every other pane exactly where it was
/// and leave a canonical tree; a pane the tree does not hold changes nothing.
///
/// # Panics
///
/// When either moves, loses or keeps the wrong pane, or leaves a tree
/// normalization would still change.
#[test]
fn model_invariants_the_layout_operations_keep_every_other_leaf() {
    let absent = u64::MAX;
    let first = u64::MAX.saturating_sub(1);
    let second = u64::MAX.saturating_sub(2);
    let mut generator = TreeGenerator::new(GENERATOR_SEED);
    for index in 0..GENERATED_TREES {
        let tree = generator.tree(GENERATED_DEPTH).normalize();
        let leaves = tree.leaves();

        let mut untouched = tree.clone();
        assert!(
            !untouched.replace_leaf(PaneId(absent), leaf(first)),
            "tree {index} claims a pane it does not hold"
        );
        assert_eq!(
            untouched, tree,
            "tree {index} changed under a pane it lacks"
        );

        let Some(chosen) = leaves.first().copied() else {
            assert_eq!(
                tree.clone().remove_leaf(PaneId(absent)),
                None,
                "tree {index} places no pane, so removal leaves none"
            );
            continue;
        };
        assert_eq!(
            tree.clone().remove_leaf(PaneId(absent)),
            Some(tree.clone()),
            "tree {index} changed under a removal of a pane it lacks"
        );

        let mut replaced = tree.clone();
        let pair = horizontal(vec![(leaf(first), 1), (leaf(second), 1)]);
        assert!(
            replaced.replace_leaf(chosen, pair),
            "tree {index} holds the pane it was asked for"
        );
        let expected: Vec<PaneId> = leaves
            .iter()
            .flat_map(|pane| {
                if *pane == chosen {
                    vec![PaneId(first), PaneId(second)]
                } else {
                    vec![*pane]
                }
            })
            .collect();
        assert_eq!(
            replaced.leaves(),
            expected,
            "tree {index} after a replacement"
        );
        assert!(
            is_canonical(&replaced, true),
            "tree {index} was left unnormalized by a replacement: {replaced:?}"
        );

        let remaining: Vec<PaneId> = leaves
            .iter()
            .copied()
            .filter(|pane| *pane != chosen)
            .collect();
        match tree.clone().remove_leaf(chosen) {
            None => assert!(
                remaining.is_empty(),
                "tree {index} still holds {remaining:?}"
            ),
            Some(pruned) => {
                assert_eq!(pruned.leaves(), remaining, "tree {index} after a removal");
                assert!(
                    is_canonical(&pruned, true),
                    "tree {index} was left unnormalized by a removal: {pruned:?}"
                );
            }
        }
    }
}

/// A tab's identity is a field, not its place: taking the first of three tabs
/// away leaves the other two carrying exactly the ids they carried.
///
/// # Panics
///
/// When a surviving tab's id or panes changed, or the model stopped holding
/// together.
#[test]
fn model_invariants_identity_is_not_positional() {
    let before = host(vec![session(
        1,
        vec![simple_tab(10, 1), simple_tab(20, 2), simple_tab(30, 3)],
    )]);
    assert_eq!(before.validate(), Ok(()), "three tabs hold together");
    let mut after = before.clone();
    after
        .sessions
        .first_mut()
        .expect("the only session")
        .tabs
        .remove(0);
    let survivors = &after.sessions.first().expect("the only session").tabs;
    assert_eq!(
        survivors.iter().map(|tab| tab.id).collect::<Vec<TabId>>(),
        [TabId(20), TabId(30)],
        "the surviving tabs keep the ids they were minted with"
    );
    assert_eq!(
        survivors[..],
        before.sessions.first().expect("the only session").tabs[1..],
        "the surviving tabs are untouched"
    );
    assert_eq!(after.validate(), Ok(()), "two tabs still hold together");
}

/// Nesting is bounded at the same depth on both sides of the codec and in the
/// validator: a tree at the bound encodes, decodes and validates, and one
/// level deeper is refused by all three rather than recursed into.
///
/// # Panics
///
/// When the bound is enforced at a different depth, or not at all.
#[test]
fn model_invariants_layout_depth_is_bounded_on_both_sides_of_the_codec() {
    let panes: Vec<u64> = (1..=u64::try_from(MAXIMUM_LAYOUT_DEPTH).unwrap_or(0)).collect();
    let deepest = alternating_layout(MAXIMUM_LAYOUT_DEPTH);
    assert_eq!(
        deepest.depth(),
        MAXIMUM_LAYOUT_DEPTH,
        "the tree at the bound"
    );
    let holding = one_tab(tab(1, &panes, deepest));
    assert_eq!(holding.validate(), Ok(()), "a tree at the bound is valid");
    let wire = encode_host_model(&holding).expect("a tree at the bound encodes");
    assert_eq!(
        decode_host_model(&wire),
        Ok(holding),
        "a tree at the bound decodes"
    );

    let over = MAXIMUM_LAYOUT_DEPTH.saturating_add(1);
    let past = alternating_layout(over);
    assert_eq!(past.depth(), over, "the tree past the bound");
    let mut deeper: Vec<u64> = panes.clone();
    deeper.push(u64::try_from(over).unwrap_or(0));
    let breaking = one_tab(tab(1, &deeper, past));
    assert_eq!(
        breaking.validate(),
        Err(ModelError::LayoutTooDeep {
            tab: TabId(1),
            depth: over
        }),
        "the validator refuses a tree past the bound"
    );
    assert_eq!(
        encode_host_model(&breaking),
        Err(MessageError::LayoutTooDeep {
            limit: MAXIMUM_LAYOUT_DEPTH
        }),
        "the encoder refuses what the decoder would"
    );

    // The cheapest deep tree on the wire is a chain of single-child splits;
    // one level short of the bound decodes, one past it is refused.
    let at = payload_with_layout(&nested_layout_bytes(MAXIMUM_LAYOUT_DEPTH.saturating_sub(1)))
        .expect("a payload at the bound");
    assert!(
        decode_host_model(&at).is_ok(),
        "a chain at the bound decodes"
    );
    let beyond = payload_with_layout(&nested_layout_bytes(MAXIMUM_LAYOUT_DEPTH))
        .expect("a payload past the bound");
    assert_eq!(
        decode_host_model(&beyond),
        Err(MessageError::LayoutTooDeep {
            limit: MAXIMUM_LAYOUT_DEPTH
        }),
        "a chain one level past the bound is refused"
    );
}

/// Whatever shape a tree has — weights of zero, splits of one child, splits
/// of none — the codec carries it back unchanged, so encoding is independent
/// of whether the model it belongs to holds together.
///
/// # Panics
///
/// When a generated tree does not survive the round trip.
#[test]
fn model_invariants_every_generated_tree_survives_the_codec() {
    let mut generator = TreeGenerator::new(GENERATOR_SEED);
    for index in 0..GENERATED_TREES {
        let layout = generator.tree(GENERATED_DEPTH);
        let panes: Vec<u64> = layout.leaves().iter().map(|pane| pane.0).collect();
        let model = one_tab(tab(1, &panes, layout));
        let wire = encode_host_model(&model).expect("a generated model encodes");
        assert_eq!(decode_host_model(&wire), Ok(model), "tree {index}");
    }
}
