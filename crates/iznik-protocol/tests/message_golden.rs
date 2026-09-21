//! The control messages against their golden: every line holds in both
//! directions, unknown capability bits and foreign protocol versions are
//! values, pane output is a borrow that the control channel refuses, every
//! refusal is provoked by a line, and an encoding fits a frame or is refused
//! before it is built.

use std::error::Error;
use std::path::Path;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::frame::MAXIMUM_PAYLOAD_LENGTH;
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence};
use iznik_protocol::message::{
    CHANNEL_CONTROL, ErrorCode, MarkKind, MessageError, PROTOCOL_VERSION, ToClient, ToServer,
    decode_to_client, decode_to_server, encode_to_client, encode_to_server, pane_output,
};
use iznik_testkit::golden;
use serde_json::Value;

/// The golden fixture, relative to this crate.
const FIXTURE: &str = "tests/fixtures/message.jsonl";

/// The bytes an `Input` message spends before its payload: the
/// discriminant, the pane id and the payload's length.
const INPUT_OVERHEAD: usize = 1 + 8 + 4;

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

/// An unsigned integer field narrowed to the width the message uses.
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

/// A bytes field: hex, or `{"repeat": hex, "count": n}` for a long payload.
///
/// # Errors
///
/// When the field is absent or neither form.
fn bytes_field(object: &Value, name: &str) -> Result<Vec<u8>, Failure> {
    let value = field(object, name)?;
    if let Some(hex) = value.as_str() {
        return Ok(golden::bytes(hex)?);
    }
    let pattern = golden::bytes(&string_field(value, "repeat")?)?;
    let count: usize = narrow_field(value, "count")?;
    Ok(pattern.repeat(count))
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
        _ => Err("a variant object has exactly one key".into()),
    }
}

/// The client-to-server message a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn to_server(value: &Value) -> Result<ToServer, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "Hello" => ToServer::Hello {
            protocol_version: narrow_field(&fields, "protocol_version")?,
            client_version: string_field(&fields, "client_version")?,
            capabilities: Capabilities::from_bits(narrow_field(&fields, "capabilities")?),
        },
        "SnapshotRequest" => ToServer::SnapshotRequest,
        "Command" => ToServer::Command {
            command_id: CommandId(integer_field(&fields, "command_id")?),
            payload: bytes_field(&fields, "payload")?,
        },
        "Subscribe" => ToServer::Subscribe {
            pane: PaneId(integer_field(&fields, "pane")?),
        },
        "Unsubscribe" => ToServer::Unsubscribe {
            pane: PaneId(integer_field(&fields, "pane")?),
        },
        "Resume" => ToServer::Resume {
            pane: PaneId(integer_field(&fields, "pane")?),
            from_sequence: Sequence(integer_field(&fields, "from_sequence")?),
        },
        "ScreenRequest" => ToServer::ScreenRequest {
            pane: PaneId(integer_field(&fields, "pane")?),
        },
        "Credit" => ToServer::Credit {
            channel: narrow_field(&fields, "channel")?,
            bytes: narrow_field(&fields, "bytes")?,
        },
        "ChannelReleased" => ToServer::ChannelReleased {
            channel: narrow_field(&fields, "channel")?,
        },
        "Input" => ToServer::Input {
            pane: PaneId(integer_field(&fields, "pane")?),
            bytes: bytes_field(&fields, "bytes")?,
        },
        "Resize" => ToServer::Resize {
            pane: PaneId(integer_field(&fields, "pane")?),
            columns: narrow_field(&fields, "columns")?,
            rows: narrow_field(&fields, "rows")?,
        },
        "Focus" => ToServer::Focus {
            pane: PaneId(integer_field(&fields, "pane")?),
        },
        "Ping" => ToServer::Ping,
        other => return Err(format!("no ToServer variant `{other}`").into()),
    })
}

/// The error code a name describes.
///
/// # Errors
///
/// When no code has the name.
fn error_code(name: &str) -> Result<ErrorCode, Failure> {
    Ok(match name {
        "ProtocolVersion" => ErrorCode::ProtocolVersion,
        "InputBacklog" => ErrorCode::InputBacklog,
        "UnknownPane" => ErrorCode::UnknownPane,
        "ChannelsExhausted" => ErrorCode::ChannelsExhausted,
        "NotSubscribed" => ErrorCode::NotSubscribed,
        other => return Err(format!("no ErrorCode `{other}`").into()),
    })
}

/// The mark kind a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn mark_kind(value: &Value) -> Result<MarkKind, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "PromptStart" => MarkKind::PromptStart,
        "CommandStart" => MarkKind::CommandStart,
        "CommandExecuted" => MarkKind::CommandExecuted,
        "CommandFinished" => {
            let status = field(&fields, "exit_status")?;
            let exit_status = if status.is_null() {
                None
            } else {
                let wide = status.as_i64().ok_or("exit_status is not an integer")?;
                Some(i32::try_from(wide)?)
            };
            MarkKind::CommandFinished { exit_status }
        }
        "WorkingDirectory" => MarkKind::WorkingDirectory {
            path: string_field(&fields, "path")?,
        },
        "Title" => MarkKind::Title {
            text: string_field(&fields, "text")?,
        },
        "AlternateScreen" => MarkKind::AlternateScreen {
            entered: field(&fields, "entered")?
                .as_bool()
                .ok_or("entered is not a boolean")?,
        },
        other => return Err(format!("no MarkKind `{other}`").into()),
    })
}

/// The server-to-client message a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn to_client(value: &Value) -> Result<ToClient, Failure> {
    let (name, fields) = variant(value)?;
    Ok(match name.as_str() {
        "Hello" => ToClient::Hello {
            protocol_version: narrow_field(&fields, "protocol_version")?,
            server_version: string_field(&fields, "server_version")?,
            capabilities: Capabilities::from_bits(narrow_field(&fields, "capabilities")?),
        },
        "Snapshot" => ToClient::Snapshot {
            generation: Generation(integer_field(&fields, "generation")?),
            payload: bytes_field(&fields, "payload")?,
        },
        "Delta" => ToClient::Delta {
            generation: Generation(integer_field(&fields, "generation")?),
            payload: bytes_field(&fields, "payload")?,
        },
        "CommandResult" => ToClient::CommandResult {
            command_id: CommandId(integer_field(&fields, "command_id")?),
            payload: bytes_field(&fields, "payload")?,
        },
        "PaneChannel" => ToClient::PaneChannel {
            pane: PaneId(integer_field(&fields, "pane")?),
            channel: narrow_field(&fields, "channel")?,
            sequence: Sequence(integer_field(&fields, "sequence")?),
        },
        "PaneDetached" => ToClient::PaneDetached {
            pane: PaneId(integer_field(&fields, "pane")?),
            channel: narrow_field(&fields, "channel")?,
        },
        "Screen" => ToClient::Screen {
            pane: PaneId(integer_field(&fields, "pane")?),
            sequence: Sequence(integer_field(&fields, "sequence")?),
            columns: narrow_field(&fields, "columns")?,
            rows: narrow_field(&fields, "rows")?,
            bytes: bytes_field(&fields, "bytes")?,
        },
        "Mark" => ToClient::Mark {
            pane: PaneId(integer_field(&fields, "pane")?),
            sequence: Sequence(integer_field(&fields, "sequence")?),
            kind: mark_kind(field(&fields, "kind")?)?,
        },
        "Pong" => ToClient::Pong,
        "Error" => ToClient::Error {
            code: error_code(&string_field(&fields, "code")?)?,
            message: string_field(&fields, "message")?,
        },
        other => return Err(format!("no ToClient variant `{other}`").into()),
    })
}

/// The refusal a JSON value describes.
///
/// # Errors
///
/// When the value does not describe one.
fn message_error(value: &Value) -> Result<MessageError, Failure> {
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
        "ControlChannel" => MessageError::ControlChannel,
        "Oversize" => MessageError::Oversize {
            length: narrow_field(&fields, "length")?,
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

/// Holds one pane-output line to the codec.
///
/// # Errors
///
/// When the line is malformed.
///
/// # Panics
///
/// When the codec does not behave as the line says.
fn check_pane_output(
    description: &str,
    pane: &Value,
    error: Option<&Value>,
) -> Result<(), Failure> {
    let channel: u8 = narrow_field(pane, "channel")?;
    let payload = bytes_field(pane, "payload")?;
    let outcome = pane_output(channel, &payload);
    if let Some(error) = error {
        assert_eq!(outcome, Err(message_error(error)?), "{description}");
        return Ok(());
    }
    let borrowed = outcome.map_err(|refusal| format!("{description}: {refusal}"))?;
    assert!(
        std::ptr::eq(borrowed.as_ptr(), payload.as_ptr()) && borrowed.len() == payload.len(),
        "{description}: pane output is not the payload itself"
    );
    Ok(())
}

/// Holds one message line to the codec, in the direction it names.
///
/// # Errors
///
/// When the line is malformed.
///
/// # Panics
///
/// When the codec does not behave as the line says.
fn check_message(description: &str, line: &Value) -> Result<(), Failure> {
    let direction = string_field(line, "direction")?;
    let expected_error = line.get("error").map(message_error).transpose()?;
    let wire = match line.get("hex") {
        Some(_present) => Some(golden::bytes(&string_field(line, "hex")?)?),
        None => None,
    };
    let message = line.get("message");
    match (direction.as_str(), message, wire, expected_error) {
        ("to_server", Some(message), Some(wire), None) => {
            let message = to_server(message)?;
            let encoded =
                encode_to_server(&message).map_err(|error| format!("{description}: {error}"))?;
            assert_eq!(
                golden::hex(&encoded),
                golden::hex(&wire),
                "{description}: encode"
            );
            assert_eq!(
                decode_to_server(&wire),
                Ok(message),
                "{description}: decode"
            );
        }
        ("to_client", Some(message), Some(wire), None) => {
            let message = to_client(message)?;
            let encoded =
                encode_to_client(&message).map_err(|error| format!("{description}: {error}"))?;
            assert_eq!(
                golden::hex(&encoded),
                golden::hex(&wire),
                "{description}: encode"
            );
            assert_eq!(
                decode_to_client(&wire),
                Ok(message),
                "{description}: decode"
            );
        }
        ("to_server", None, Some(wire), Some(error)) => {
            assert_eq!(decode_to_server(&wire), Err(error), "{description}");
        }
        ("to_client", None, Some(wire), Some(error)) => {
            assert_eq!(decode_to_client(&wire), Err(error), "{description}");
        }
        ("to_server", Some(message), None, Some(error)) => {
            assert_eq!(
                encode_to_server(&to_server(message)?),
                Err(error),
                "{description}"
            );
        }
        ("to_client", Some(message), None, Some(error)) => {
            assert_eq!(
                encode_to_client(&to_client(message)?),
                Err(error),
                "{description}"
            );
        }
        _ => return Err(format!("{description}: not a shape this test knows").into()),
    }
    Ok(())
}

/// Every fixture line is the contract, in the direction it names.
///
/// # Panics
///
/// When a line is malformed, or the codec does not behave as it says; the
/// message names the line's description.
#[test]
fn message_golden_every_line_holds_in_both_directions() {
    let lines = lines().expect("the fixture loads");
    assert!(lines.len() > 50, "the fixture has {} lines", lines.len());
    for line in &lines {
        let description = string_field(line, "description").expect("a description");
        let outcome = match line.get("pane_output") {
            Some(pane) => check_pane_output(&description, pane, line.get("error")),
            None => check_message(&description, line),
        };
        outcome.unwrap_or_else(|error| panic!("{description}: {error}"));
    }
}

/// Unknown capability bits survive a decode-then-encode round trip.
///
/// # Panics
///
/// When a bit is dropped or reported as known.
#[test]
fn message_golden_unknown_capability_bits_survive_a_round_trip() {
    let advertised = Capabilities::from_bits(0x8000_000F);
    assert_eq!(advertised.unknown_bits(), 0x8000_0008);
    assert_eq!(Capabilities::ZSTD.unknown_bits(), 0);
    assert_eq!(Capabilities::RESUME.unknown_bits(), 0);
    assert_eq!(Capabilities::REORDER_SESSIONS.unknown_bits(), 0);
    let hello = ToServer::Hello {
        protocol_version: PROTOCOL_VERSION,
        client_version: "future".to_owned(),
        capabilities: advertised,
    };
    let wire = encode_to_server(&hello).expect("a Hello encodes");
    let decoded = decode_to_server(&wire).expect("a Hello decodes");
    assert_eq!(decoded, hello);
    assert_eq!(encode_to_server(&decoded).expect("re-encodes"), wire);
}

/// A protocol version other than 1 is decoded as a value, not refused.
///
/// # Panics
///
/// When the codec refuses it or reports another value.
#[test]
fn message_golden_a_foreign_protocol_version_is_a_value() {
    assert_eq!(PROTOCOL_VERSION, 1);
    let hello = ToClient::Hello {
        protocol_version: 7,
        server_version: "iznik-server 9".to_owned(),
        capabilities: Capabilities::from_bits(0),
    };
    let wire = encode_to_client(&hello).expect("a Hello encodes");
    match decode_to_client(&wire) {
        Ok(ToClient::Hello {
            protocol_version, ..
        }) => assert_eq!(protocol_version, 7),
        other => panic!("a foreign version was not decoded as a value: {other:?}"),
    }
}

/// Pane output is a borrow of the payload, and the control channel refuses.
///
/// # Panics
///
/// When the borrow is not the payload itself, or channel 0 is not refused.
#[test]
fn message_golden_pane_output_borrows_and_refuses_the_control_channel() {
    let payload = vec![0x1b, b'[', b'A'];
    let borrowed = pane_output(1, &payload).expect("channel 1 carries pane output");
    assert!(std::ptr::eq(borrowed.as_ptr(), payload.as_ptr()));
    assert_eq!(borrowed.len(), payload.len());
    assert_eq!(
        pane_output(CHANNEL_CONTROL, &payload),
        Err(MessageError::ControlChannel)
    );
}

/// Every refusal the codec can give is provoked by a fixture line.
///
/// # Panics
///
/// When a `MessageError` variant has no line.
#[test]
fn message_golden_every_refusal_is_provoked_by_a_line() {
    let lines = lines().expect("the fixture loads");
    let mut provoked: Vec<String> = lines
        .iter()
        .filter_map(|line| line.get("error"))
        .map(|error| variant(error).expect("an error variant").0)
        .collect();
    provoked.sort();
    provoked.dedup();
    let expected = [
        "ControlChannel",
        "Oversize",
        "TrailingBytes",
        "Truncated",
        "UnknownDiscriminant",
        "Utf8",
    ];
    assert_eq!(provoked, expected);
}

/// An encoding fits a frame, or is refused as oversize before it is built.
///
/// # Panics
///
/// When the boundary is off by one in either direction.
#[test]
fn message_golden_an_encoding_fits_a_frame_or_is_refused() {
    let maximum = usize::try_from(MAXIMUM_PAYLOAD_LENGTH).expect("the maximum fits");
    let room = maximum
        .checked_sub(INPUT_OVERHEAD)
        .expect("the overhead is below the maximum");
    let fitting = ToServer::Input {
        pane: PaneId(1),
        bytes: vec![0; room],
    };
    let wire = encode_to_server(&fitting).expect("an encoding of exactly the maximum fits");
    assert_eq!(wire.len(), maximum);
    let one_over = ToServer::Input {
        pane: PaneId(1),
        bytes: vec![0; room.checked_add(1).expect("one more fits a usize")],
    };
    assert_eq!(
        encode_to_server(&one_over),
        Err(MessageError::Oversize {
            length: maximum.checked_add(1).expect("one more fits a usize")
        })
    );
}
