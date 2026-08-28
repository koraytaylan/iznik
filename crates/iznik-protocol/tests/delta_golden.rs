//! The deltas against their golden: every line encodes and decodes exactly,
//! every refusal the decoder can give is provoked by a payload, and an encoded
//! delta rides inside the `Delta` message that plan 0001 left opaque.

use std::error::Error;
use std::path::Path;

use iznik_protocol::delta::{Delta, ExitStatus, RemovalReason, decode_delta, encode_delta};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::{MessageError, ToClient, decode_to_client, encode_to_client};
use iznik_protocol::model::{LayoutNode, Pane, Session, SplitDirection, Tab, Weighted};
use iznik_testkit::golden;
use serde_json::Value;

/// The golden fixture, relative to this crate.
const FIXTURE: &str = "tests/fixtures/delta.jsonl";

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

/// The session a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn session_of(value: &Value) -> Result<Session, Failure> {
    let mut tabs = Vec::new();
    for tab in array_field(value, "tabs")? {
        tabs.push(tab_of(tab)?);
    }
    Ok(Session {
        id: SessionId(integer_field(value, "id")?),
        name: string_field(value, "name")?,
        tabs,
    })
}

/// The exit status a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn exit_status_of(value: &Value) -> Result<ExitStatus, Failure> {
    let (name, fields) = variant(value)?;
    let number = fields.as_i64().ok_or("an exit status is a number")?;
    let narrowed = i32::try_from(number)?;
    match name.as_str() {
        "Exited" => Ok(ExitStatus::Exited(narrowed)),
        "Signalled" => Ok(ExitStatus::Signalled(narrowed)),
        other => Err(format!("no ExitStatus `{other}`").into()),
    }
}

/// The removal reason a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn reason_of(value: &Value) -> Result<RemovalReason, Failure> {
    let (name, fields) = variant(value)?;
    match name.as_str() {
        "Closed" => Ok(RemovalReason::Closed),
        "Exited" => Ok(RemovalReason::Exited(exit_status_of(&fields)?)),
        other => Err(format!("no RemovalReason `{other}`").into()),
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

/// The delta a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn delta_of(value: &Value) -> Result<Delta, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "SessionAdded" => Delta::SessionAdded {
            session: session_of(field(&fields, "session")?)?,
        },
        "SessionRenamed" => Delta::SessionRenamed {
            session: SessionId(integer_field(&fields, "session")?),
            name: string_field(&fields, "name")?,
        },
        "SessionRemoved" => Delta::SessionRemoved {
            session: SessionId(integer_field(&fields, "session")?),
        },
        "TabAdded" => Delta::TabAdded {
            session: SessionId(integer_field(&fields, "session")?),
            tab: tab_of(field(&fields, "tab")?)?,
            index: narrow_field(&fields, "index")?,
        },
        "TabRenamed" => Delta::TabRenamed {
            tab: TabId(integer_field(&fields, "tab")?),
            name: string_field(&fields, "name")?,
        },
        "TabRemoved" => Delta::TabRemoved {
            tab: TabId(integer_field(&fields, "tab")?),
        },
        "TabsReordered" => Delta::TabsReordered {
            session: SessionId(integer_field(&fields, "session")?),
            order: order_of(&fields, "order")?,
        },
        "PaneAdded" => Delta::PaneAdded {
            tab: TabId(integer_field(&fields, "tab")?),
            pane: pane_of(field(&fields, "pane")?)?,
        },
        "PaneRemoved" => Delta::PaneRemoved {
            pane: PaneId(integer_field(&fields, "pane")?),
            reason: reason_of(field(&fields, "reason")?)?,
        },
        "PaneMoved" => Delta::PaneMoved {
            pane: PaneId(integer_field(&fields, "pane")?),
            to_tab: TabId(integer_field(&fields, "to_tab")?),
        },
        "LayoutChanged" => Delta::LayoutChanged {
            tab: TabId(integer_field(&fields, "tab")?),
            layout: layout_of(field(&fields, "layout")?)?,
        },
        "PaneTitle" => Delta::PaneTitle {
            pane: PaneId(integer_field(&fields, "pane")?),
            title: string_field(&fields, "title")?,
        },
        "PaneWorkingDirectory" => Delta::PaneWorkingDirectory {
            pane: PaneId(integer_field(&fields, "pane")?),
            path: string_field(&fields, "path")?,
        },
        "PaneResized" => Delta::PaneResized {
            pane: PaneId(integer_field(&fields, "pane")?),
            columns: narrow_field(&fields, "columns")?,
            rows: narrow_field(&fields, "rows")?,
        },
        other => return Err(format!("no Delta variant `{other}`").into()),
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

/// Holds one line to the codec, in whichever way it names.
///
/// # Errors
///
/// When the line is malformed.
///
/// # Panics
///
/// When the codec does not behave as the line says.
fn check(description: &str, line: &Value) -> Result<(), Failure> {
    let wire = golden::bytes(&string_field(line, "hex")?)?;
    if let Some(error) = line.get("error") {
        assert_eq!(
            decode_delta(&wire),
            Err(message_error_of(error)?),
            "{description}"
        );
        return Ok(());
    }
    let delta = delta_of(field(line, "delta")?)?;
    let encoded = encode_delta(&delta).map_err(|error| format!("{description}: {error}"))?;
    assert_eq!(
        golden::hex(&encoded),
        golden::hex(&wire),
        "{description}: encode"
    );
    assert_eq!(decode_delta(&wire), Ok(delta), "{description}: decode");
    Ok(())
}

/// Every fixture line is the contract, in both directions.
///
/// # Panics
///
/// When a line is malformed, or the codec does not behave as it says.
#[test]
fn delta_golden_every_line_holds_in_both_directions() {
    let lines = lines().expect("the fixture loads");
    assert!(lines.len() > 20, "the fixture has {} lines", lines.len());
    for line in &lines {
        let description = string_field(line, "description").expect("a description");
        check(&description, line).unwrap_or_else(|error| panic!("{description}: {error}"));
    }
}

/// Every `Delta` variant has a line, so no variant is pinned by nothing.
///
/// # Panics
///
/// When a variant has no line.
#[test]
fn delta_golden_every_variant_has_a_line() {
    let lines = lines().expect("the fixture loads");
    let mut named: Vec<String> = lines
        .iter()
        .filter_map(|line| line.get("delta"))
        .map(|delta| variant(delta).expect("a delta variant").0)
        .collect();
    named.sort();
    named.dedup();
    assert_eq!(
        named,
        [
            "LayoutChanged",
            "PaneAdded",
            "PaneMoved",
            "PaneRemoved",
            "PaneResized",
            "PaneTitle",
            "PaneWorkingDirectory",
            "SessionAdded",
            "SessionRemoved",
            "SessionRenamed",
            "TabAdded",
            "TabRemoved",
            "TabRenamed",
            "TabsReordered",
        ],
        "the variants the fixture pins"
    );
}

/// Every refusal the decoder can give is provoked by a line.
///
/// # Panics
///
/// When a refusal has no line.
#[test]
fn delta_golden_every_refusal_is_provoked_by_a_line() {
    let lines = lines().expect("the fixture loads");
    let mut provoked: Vec<String> = lines
        .iter()
        .filter_map(|line| line.get("error"))
        .map(|error| variant(error).expect("an error variant").0)
        .collect();
    provoked.sort();
    provoked.dedup();
    assert_eq!(
        provoked,
        ["TrailingBytes", "Truncated", "UnknownDiscriminant", "Utf8"],
        "the refusals the fixture provokes"
    );
}

/// An encoded delta is the payload the `Delta` message carries: plan 0001 left
/// it opaque, and this is what fills it.
///
/// # Panics
///
/// When the payload does not survive the message it rides in.
#[test]
fn delta_golden_an_encoded_delta_rides_inside_its_message() {
    let lines = lines().expect("the fixture loads");
    for line in &lines {
        let Some(value) = line.get("delta") else {
            continue;
        };
        let delta = delta_of(value).expect("a delta");
        let payload = encode_delta(&delta).expect("a delta encodes");
        let message = ToClient::Delta {
            generation: Generation(7),
            payload,
        };
        let wire = encode_to_client(&message).expect("the message encodes");
        match decode_to_client(&wire) {
            Ok(ToClient::Delta {
                generation,
                payload: carried,
            }) => {
                assert_eq!(generation, Generation(7), "the generation rides along");
                assert_eq!(decode_delta(&carried), Ok(delta), "the delta rides along");
            }
            other => panic!("a Delta message did not come back: {other:?}"),
        }
    }
}
