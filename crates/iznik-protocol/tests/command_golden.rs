//! The session commands and their outcomes against their golden: every line
//! encodes and decodes exactly, every variant and every form has a line, every
//! refusal the decoders can give is provoked, and an encoded command rides
//! inside the `Command` message that plan 0001 left opaque.

use std::error::Error;
use std::path::Path;

use iznik_protocol::command::{
    CommandOutcome, Created, Placement, RejectionCode, SessionCommand, decode_command_outcome,
    decode_session_command, encode_command_outcome, encode_session_command,
};
use iznik_protocol::identity::{CommandId, Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::{
    MessageError, ToClient, ToServer, decode_to_client, decode_to_server, encode_to_client,
    encode_to_server,
};
use iznik_protocol::model::{LayoutNode, MAXIMUM_LAYOUT_DEPTH, SplitDirection, Weighted};
use iznik_testkit::golden;
use serde_json::Value;

/// The golden fixture, relative to this crate.
const FIXTURE: &str = "tests/fixtures/command.jsonl";

/// The command id the message round trip rides under.
const RIDER: CommandId = CommandId(11);

/// Why a line could not be read.
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

/// An unsigned integer field narrowed to the width the delta uses.
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

/// The order a JSON array of tab ids describes.
///
/// # Errors
///
/// When the value is not an array of unsigned integers.
fn order_of(value: &Value, name: &str) -> Result<Vec<TabId>, Failure> {
    array_field(value, name)?
        .iter()
        .map(|tab| {
            tab.as_u64()
                .map(TabId)
                .ok_or_else(|| "an order names tab ids".into())
        })
        .collect()
}

/// An optional string field: a string, or null for absent.
///
/// # Errors
///
/// When the field is absent or is neither a string nor null.
fn optional_field(object: &Value, name: &str) -> Result<Option<String>, Failure> {
    match field(object, name)? {
        Value::Null => Ok(None),
        other => Ok(Some(
            other
                .as_str()
                .ok_or("a directory is a string or null")?
                .to_owned(),
        )),
    }
}

/// The direction a name describes.
///
/// # Errors
///
/// When no direction has the name.
fn direction_of(name: &str) -> Result<SplitDirection, Failure> {
    match name {
        "Horizontal" => Ok(SplitDirection::Horizontal),
        "Vertical" => Ok(SplitDirection::Vertical),
        other => Err(format!("no SplitDirection `{other}`").into()),
    }
}

/// The placement a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn placement_of(value: &Value) -> Result<Placement, Failure> {
    Ok(Placement {
        target: PaneId(integer_field(value, "target")?),
        direction: direction_of(&string_field(value, "direction")?)?,
        before: field(value, "before")?
            .as_bool()
            .ok_or("a side is a boolean")?,
    })
}

/// The command a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn command_of(value: &Value) -> Result<SessionCommand, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "CreateSession" => SessionCommand::CreateSession {
            name: string_field(&fields, "name")?,
            columns: narrow_field(&fields, "columns")?,
            rows: narrow_field(&fields, "rows")?,
            working_directory: optional_field(&fields, "working_directory")?,
        },
        "RenameSession" => SessionCommand::RenameSession {
            session: SessionId(integer_field(&fields, "session")?),
            name: string_field(&fields, "name")?,
        },
        "CloseSession" => SessionCommand::CloseSession {
            session: SessionId(integer_field(&fields, "session")?),
        },
        "CreateTab" => SessionCommand::CreateTab {
            session: SessionId(integer_field(&fields, "session")?),
            name: string_field(&fields, "name")?,
            columns: narrow_field(&fields, "columns")?,
            rows: narrow_field(&fields, "rows")?,
            working_directory: optional_field(&fields, "working_directory")?,
        },
        "RenameTab" => SessionCommand::RenameTab {
            tab: TabId(integer_field(&fields, "tab")?),
            name: string_field(&fields, "name")?,
        },
        "CloseTab" => SessionCommand::CloseTab {
            tab: TabId(integer_field(&fields, "tab")?),
        },
        "ReorderTabs" => SessionCommand::ReorderTabs {
            session: SessionId(integer_field(&fields, "session")?),
            order: order_of(&fields, "order")?,
        },
        "CreatePane" => SessionCommand::CreatePane {
            tab: TabId(integer_field(&fields, "tab")?),
            placement: placement_of(field(&fields, "placement")?)?,
            columns: narrow_field(&fields, "columns")?,
            rows: narrow_field(&fields, "rows")?,
            working_directory: optional_field(&fields, "working_directory")?,
        },
        "ClosePane" => SessionCommand::ClosePane {
            pane: PaneId(integer_field(&fields, "pane")?),
        },
        "MovePane" => SessionCommand::MovePane {
            pane: PaneId(integer_field(&fields, "pane")?),
            to_tab: TabId(integer_field(&fields, "to_tab")?),
            placement: placement_of(field(&fields, "placement")?)?,
        },
        "SetLayout" => SessionCommand::SetLayout {
            tab: TabId(integer_field(&fields, "tab")?),
            layout: layout_of(field(&fields, "layout")?)?,
        },
        other => return Err(format!("no SessionCommand variant `{other}`").into()),
    })
}

/// What a command made, as a JSON value describes it.
///
/// # Errors
///
/// When the value does not describe one.
fn created_of(value: &Value) -> Result<Created, Failure> {
    let (name, fields) = variant(value)?;
    let named = || -> Result<u64, Failure> {
        fields
            .as_u64()
            .ok_or_else(|| "a created form names an id".into())
    };
    match name.as_str() {
        "Nothing" => Ok(Created::Nothing),
        "Session" => Ok(Created::Session(SessionId(named()?))),
        "Tab" => Ok(Created::Tab(TabId(named()?))),
        "Pane" => Ok(Created::Pane(PaneId(named()?))),
        other => Err(format!("no Created variant `{other}`").into()),
    }
}

/// The rejection code a name describes.
///
/// # Errors
///
/// When no code has the name.
fn rejection_of(name: &str) -> Result<RejectionCode, Failure> {
    Ok(match name {
        "UnknownSession" => RejectionCode::UnknownSession,
        "UnknownTab" => RejectionCode::UnknownTab,
        "UnknownPane" => RejectionCode::UnknownPane,
        "EmptyName" => RejectionCode::EmptyName,
        "InvalidOrder" => RejectionCode::InvalidOrder,
        "InvalidLayout" => RejectionCode::InvalidLayout,
        "SpawnFailed" => RejectionCode::SpawnFailed,
        other => return Err(format!("no RejectionCode `{other}`").into()),
    })
}

/// The outcome a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn outcome_of(value: &Value) -> Result<CommandOutcome, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "Applied" => CommandOutcome::Applied {
            generation: Generation(integer_field(&fields, "generation")?),
            created: created_of(field(&fields, "created")?)?,
        },
        "Rejected" => CommandOutcome::Rejected {
            code: rejection_of(&string_field(&fields, "code")?)?,
            message: string_field(&fields, "message")?,
        },
        other => return Err(format!("no CommandOutcome variant `{other}`").into()),
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

/// Holds one line to the codec it names.
///
/// # Errors
///
/// When the line is malformed.
///
/// # Panics
///
/// When a codec does not behave as the line says.
fn check(description: &str, line: &Value) -> Result<(), Failure> {
    let wire = golden::bytes(&string_field(line, "hex")?)?;
    if let Some(error) = line.get("error") {
        let refusal = message_error_of(error)?;
        let outcome = match string_field(line, "kind")?.as_str() {
            "command" => decode_session_command(&wire).err(),
            "outcome" => decode_command_outcome(&wire).err(),
            other => return Err(format!("no codec `{other}`").into()),
        };
        assert_eq!(outcome, Some(refusal), "{description}");
        return Ok(());
    }
    if let Some(value) = line.get("command") {
        let command = command_of(value)?;
        let encoded =
            encode_session_command(&command).map_err(|error| format!("{description}: {error}"))?;
        assert_eq!(golden::hex(&encoded), golden::hex(&wire), "{description}");
        assert_eq!(decode_session_command(&wire), Ok(command), "{description}");
        return Ok(());
    }
    let outcome = outcome_of(field(line, "outcome")?)?;
    let encoded =
        encode_command_outcome(&outcome).map_err(|error| format!("{description}: {error}"))?;
    assert_eq!(golden::hex(&encoded), golden::hex(&wire), "{description}");
    assert_eq!(decode_command_outcome(&wire), Ok(outcome), "{description}");
    Ok(())
}

/// The refusals the fixture provokes on one side of the codec, sorted. Each
/// decoder is held to its own, because pooling them lets one side's line stand
/// in for the other's missing one.
///
/// # Errors
///
/// When a line is malformed.
fn refusals_of(side: &str) -> Result<Vec<String>, Failure> {
    let mut names = Vec::new();
    for line in &lines()? {
        let Some(error) = line.get("error") else {
            continue;
        };
        if string_field(line, "kind")? == side {
            names.push(variant(error)?.0);
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// The names of the variants the fixture's lines under `key` describe.
///
/// # Errors
///
/// When a line is malformed.
fn named(key: &str) -> Result<Vec<String>, Failure> {
    let mut names: Vec<String> = lines()?
        .iter()
        .filter_map(|line| line.get(key))
        .map(|value| variant(value).map(|(name, _fields)| name))
        .collect::<Result<Vec<String>, Failure>>()?;
    names.sort();
    names.dedup();
    Ok(names)
}

/// Every fixture line is the contract, in both directions.
///
/// # Panics
///
/// When a line is malformed, or a codec does not behave as it says.
#[test]
fn command_golden_every_line_holds_in_both_directions() {
    let lines = lines().expect("the fixture loads");
    assert!(lines.len() > 30, "the fixture has {} lines", lines.len());
    for line in &lines {
        let description = string_field(line, "description").expect("a description");
        check(&description, line).unwrap_or_else(|error| panic!("{description}: {error}"));
    }
}

/// Every command variant has a line, so no variant is pinned by nothing.
///
/// # Panics
///
/// When a variant has no line.
#[test]
fn command_golden_every_command_variant_has_a_line() {
    assert_eq!(
        named("command").expect("the fixture loads"),
        [
            "ClosePane",
            "CloseSession",
            "CloseTab",
            "CreatePane",
            "CreateSession",
            "CreateTab",
            "MovePane",
            "RenameSession",
            "RenameTab",
            "ReorderTabs",
            "SetLayout",
        ],
        "the commands the fixture pins"
    );
}

/// Every `Created` form and every `RejectionCode` has a line.
///
/// # Panics
///
/// When a form has no line.
#[test]
fn command_golden_every_outcome_form_has_a_line() {
    let lines = lines().expect("the fixture loads");
    let mut created: Vec<String> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    for line in &lines {
        let Some(value) = line.get("outcome") else {
            continue;
        };
        match outcome_of(value).expect("an outcome") {
            // The name, not the value: `Session(3)` and `Session(4)` are one
            // form, and counting them as two would leave a wire byte pinned
            // by nothing while the test stayed green.
            CommandOutcome::Applied { created: made, .. } => created.push(
                match made {
                    Created::Nothing => "Nothing",
                    Created::Session(_named) => "Session",
                    Created::Tab(_named) => "Tab",
                    Created::Pane(_named) => "Pane",
                }
                .to_owned(),
            ),
            CommandOutcome::Rejected { code, .. } => rejected.push(format!("{code:?}")),
        }
    }
    created.sort();
    created.dedup();
    rejected.sort();
    rejected.dedup();
    assert_eq!(
        created,
        ["Nothing", "Pane", "Session", "Tab"],
        "the created forms the fixture pins"
    );
    assert_eq!(
        rejected,
        [
            "EmptyName",
            "InvalidLayout",
            "InvalidOrder",
            "SpawnFailed",
            "UnknownPane",
            "UnknownSession",
            "UnknownTab",
        ],
        "the rejection codes the fixture pins"
    );
}

/// Every refusal the decoders can give is provoked: four by a fixture line on
/// one side or the other, and the fifth by a layout nested past what a model
/// holds — which the encoder refuses to write, so nothing it produces is
/// something the decoder turns away.
///
/// # Panics
///
/// When a refusal has nothing that provokes it, or the encoder writes a tree
/// its own decoder would refuse.
#[test]
fn command_golden_every_refusal_the_decoders_give_is_provoked() {
    let expected = [
        "TrailingBytes".to_owned(),
        "Truncated".to_owned(),
        "UnknownDiscriminant".to_owned(),
        "Utf8".to_owned(),
    ];
    for side in ["command", "outcome"] {
        assert_eq!(
            refusals_of(side).expect("the fixture loads"),
            expected,
            "the refusals the {side} decoder is provoked into"
        );
    }
    let mut provoked = named("error").expect("the fixture loads");
    let refusal = MessageError::LayoutTooDeep {
        limit: MAXIMUM_LAYOUT_DEPTH,
    };
    let mut layout = LayoutNode::Leaf(PaneId(5));
    let shallow = encode_session_command(&SessionCommand::SetLayout {
        tab: TabId(3),
        layout: layout.clone(),
    })
    .expect("a shallow layout encodes");
    for _index in 0..MAXIMUM_LAYOUT_DEPTH {
        layout = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![Weighted {
                node: layout,
                weight: 1,
            }],
        };
    }
    assert_eq!(
        encode_session_command(&SessionCommand::SetLayout {
            tab: TabId(3),
            layout
        }),
        Err(refusal.clone()),
        "the encoder wrote a tree its decoder refuses"
    );
    let deep = deeply_nested_payload(&shallow).expect("a deep payload");
    assert_eq!(
        decode_session_command(&deep),
        Err(refusal),
        "a tree past the bound"
    );
    provoked.push("LayoutTooDeep".to_owned());
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

/// A `SetLayout` payload one level past the bound, built by wrapping the
/// layout of one the encoder did write.
///
/// # Errors
///
/// When the shallow payload does not end in a leaf.
fn deeply_nested_payload(shallow: &[u8]) -> Result<Vec<u8>, Failure> {
    /// A `Split`, `Horizontal`, holding one child.
    const SPLIT: [u8; 6] = [0, 0, 1, 0, 0, 0];
    /// The weight that child is held at.
    const WEIGHT: [u8; 4] = [1, 0, 0, 0];
    /// A leaf's bytes: its tag and the pane it names.
    const LEAF_LENGTH: usize = size_of::<u8>() + size_of::<u64>();

    let cut = shallow
        .len()
        .checked_sub(LEAF_LENGTH)
        .ok_or("a leaf's bytes")?;
    let (prefix, leaf) = shallow.split_at_checked(cut).ok_or("the layout's place")?;
    let mut layout = leaf.to_vec();
    for _index in 0..MAXIMUM_LAYOUT_DEPTH {
        let mut wrapped = SPLIT.to_vec();
        wrapped.append(&mut layout);
        wrapped.extend_from_slice(&WEIGHT);
        layout = wrapped;
    }
    let mut bytes = prefix.to_vec();
    bytes.append(&mut layout);
    Ok(bytes)
}

/// An encoded command is the payload the `Command` message carries: plan 0001
/// left it opaque, and this is what fills it.
///
/// # Panics
///
/// When the payload does not survive the message it rides in.
#[test]
fn command_golden_an_encoded_command_rides_inside_its_message() {
    for line in &lines().expect("the fixture loads") {
        let Some(value) = line.get("command") else {
            continue;
        };
        let command = command_of(value).expect("a command");
        let payload = encode_session_command(&command).expect("a command encodes");
        let message = ToServer::Command {
            command_id: RIDER,
            payload,
        };
        let wire = encode_to_server(&message).expect("the message encodes");
        match decode_to_server(&wire) {
            Ok(ToServer::Command {
                command_id,
                payload: carried,
            }) => {
                assert_eq!(command_id, RIDER, "the command id rides along");
                assert_eq!(
                    decode_session_command(&carried),
                    Ok(command),
                    "the command rides along"
                );
            }
            other => panic!("a Command message did not come back: {other:?}"),
        }
    }
}

/// An encoded outcome is the payload the `CommandResult` message carries.
///
/// # Panics
///
/// When the payload does not survive the message it rides in.
#[test]
fn command_golden_an_encoded_outcome_rides_inside_its_message() {
    for line in &lines().expect("the fixture loads") {
        let Some(value) = line.get("outcome") else {
            continue;
        };
        let outcome = outcome_of(value).expect("an outcome");
        let payload = encode_command_outcome(&outcome).expect("an outcome encodes");
        let message = ToClient::CommandResult {
            command_id: RIDER,
            payload,
        };
        let wire = encode_to_client(&message).expect("the message encodes");
        match decode_to_client(&wire) {
            Ok(ToClient::CommandResult {
                command_id,
                payload: carried,
            }) => {
                assert_eq!(command_id, RIDER, "the command id rides along");
                assert_eq!(
                    decode_command_outcome(&carried),
                    Ok(outcome),
                    "the outcome rides along"
                );
            }
            other => panic!("a CommandResult did not come back: {other:?}"),
        }
    }
}
