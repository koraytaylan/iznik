//! The protocol client held to the goldens: every frame it sends is the one
//! `message.jsonl` pins for that message, byte for byte, and everything it is
//! told it decodes, records and answers.
//!
//! Its server side is a script over `tokio::io::duplex` — no daemon, no
//! socket, no shell — so the whole file runs in milliseconds and every
//! deadline in it is well under a second.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iznik_link::compression::compressed;
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::delta::Delta;
use iznik_protocol::frame;
use iznik_protocol::identity::{Generation, PaneId, Sequence};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
    encode_to_server,
};
use iznik_testkit::client::{ClientError, Received, ServerHello, TestClient};
use iznik_testkit::golden;
use serde_json::Value;
use tokio::io::{AsyncReadExt, DuplexStream};

/// The golden the frames are held to, from this crate.
const FIXTURE: &str = "../iznik-protocol/tests/fixtures/message.jsonl";

/// How much the duplex pair holds before a write waits.
const PIPE_CAPACITY: usize = 1 << 20;

/// The deadline a case gives something that should already be there.
const PROMPT: Duration = Duration::from_millis(200);

/// The deadline a case gives something that will never come.
const BRIEF: Duration = Duration::from_millis(50);

/// How much later than its deadline a refusal may arrive.
const SLACK: Duration = Duration::from_millis(400);

/// The pane every case names.
const PANE: PaneId = PaneId(7);

/// How much input the compression case sends, to show a real payload survives
/// the round trip and not only a bare `Ping`.
const TYPED_BYTES: usize = 4096;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A client and the script on the other end of its stream.
struct Pair {
    /// The thing under test.
    client: TestClient<DuplexStream>,
    /// The server side, unframed, so a case can read exactly the bytes that
    /// went out.
    server: DuplexStream,
}

/// A client over a duplex pair, with the other end raw.
fn pair() -> Pair {
    let (near, far) = tokio::io::duplex(PIPE_CAPACITY);
    Pair {
        client: TestClient::over(near),
        server: far,
    }
}

/// The frame bytes a payload on the control channel goes out as.
///
/// # Errors
///
/// When the payload will not fit a frame.
fn framed(payload: &[u8]) -> Result<Vec<u8>, Failed> {
    let mut out = Vec::new();
    frame::encode(CHANNEL_CONTROL, payload, &mut out)?;
    Ok(out)
}

/// Reads exactly the bytes one frame of `payload` occupies, and says what they
/// were.
///
/// # Errors
///
/// When the stream ends first.
async fn read_frame(server: &mut DuplexStream, payload: &[u8]) -> Result<Vec<u8>, Failed> {
    let mut seen = vec![0; framed(payload)?.len()];
    server.read_exact(&mut seen).await?;
    Ok(seen)
}

/// The golden's lines.
///
/// # Errors
///
/// When the fixture cannot be read.
fn fixture() -> Result<Vec<Value>, Failed> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    Ok(golden::lines(Path::new(&path))?)
}

/// The one field of a fixture line's message object: its name, and the object
/// under it. A message with no fields is its own name and an empty object.
fn shaped(line: &Value) -> Option<(String, Value)> {
    let message = line.get("message")?;
    if let Some(named) = message.as_str() {
        return Some((named.to_owned(), Value::Object(serde_json::Map::new())));
    }
    let object = message.as_object()?;
    let (named, fields) = object.iter().next()?;
    Some((named.clone(), fields.clone()))
}

/// A `u64` field of a fixture line's message.
fn number(fields: &Value, named: &str) -> Option<u64> {
    fields.get(named)?.as_u64()
}

/// A byte-string field, written in the golden as hex.
fn hexed(fields: &Value, named: &str) -> Option<Vec<u8>> {
    golden::bytes(fields.get(named)?.as_str()?).ok()
}

/// Drives the client to send the message a fixture line describes, and says
/// which kind it sent — or nothing when the line names a message whose fields
/// the client does not take from its caller.
///
/// # Errors
///
/// When the client refuses to send.
async fn send_as(
    client: &mut TestClient<DuplexStream>,
    line: &Value,
) -> Result<Option<String>, Failed> {
    let Some((named, fields)) = shaped(line) else {
        return Ok(None);
    };
    let pane = number(&fields, "pane").map(PaneId);
    match (named.as_str(), pane) {
        ("SnapshotRequest", _none) => client.snapshot_request().await?,
        ("Subscribe", Some(pane)) => client.subscribe(pane).await?,
        ("Unsubscribe", Some(pane)) => client.unsubscribe(pane).await?,
        ("ScreenRequest", Some(pane)) => client.screen_request(pane).await?,
        ("Focus", Some(pane)) => client.focus(pane).await?,
        ("Ping", _none) => client.ping().await?,
        ("Resume", Some(pane)) => {
            let from = Sequence(number(&fields, "from_sequence").unwrap_or_default());
            client.resume(pane, from).await?;
        }
        ("Credit", _none) => {
            let channel = u8::try_from(number(&fields, "channel").unwrap_or_default())?;
            let bytes = u32::try_from(number(&fields, "bytes").unwrap_or_default())?;
            client.credit(channel, bytes).await?;
        }
        ("ChannelReleased", _none) => {
            let channel = u8::try_from(number(&fields, "channel").unwrap_or_default())?;
            client.channel_released(channel).await?;
        }
        ("Input", Some(pane)) => {
            client
                .input(pane, hexed(&fields, "bytes").unwrap_or_default())
                .await?;
        }
        ("Resize", Some(pane)) => {
            let columns = u16::try_from(number(&fields, "columns").unwrap_or_default())?;
            let rows = u16::try_from(number(&fields, "rows").unwrap_or_default())?;
            client.resize(pane, columns, rows).await?;
        }
        // `Hello` carries this client's own version and `Command` its own
        // numbering, so neither is a frame a caller can ask for exactly.
        _own => return Ok(None),
    }
    Ok(Some(named))
}

/// # Panics
///
/// When a frame the client sends is not the one the golden pins for that
/// message, or when it covers fewer kinds of message than it can send.
#[tokio::test]
async fn every_frame_it_sends_is_the_golden() {
    let case = async {
        let mut sent = BTreeSet::new();
        for line in fixture()? {
            if line.get("direction").and_then(Value::as_str) != Some("to_server") {
                continue;
            }
            let Some(hex) = line.get("hex").and_then(Value::as_str) else {
                continue;
            };
            let payload = golden::bytes(hex)?;
            let mut pair = pair();
            let Some(named) = send_as(&mut pair.client, &line).await? else {
                continue;
            };
            let seen = read_frame(&mut pair.server, &payload).await?;
            assert_eq!(
                golden::hex(&seen),
                golden::hex(&framed(&payload)?),
                "{}: {}",
                named,
                line.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            );
            let _first = sent.insert(named);
        }
        let expected: BTreeSet<String> = [
            "SnapshotRequest",
            "Subscribe",
            "Unsubscribe",
            "Resume",
            "ScreenRequest",
            "Credit",
            "ChannelReleased",
            "Input",
            "Resize",
            "Focus",
            "Ping",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(sent, expected, "every message a caller can ask for exactly");
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// A server side that speaks frames.
fn framing(server: DuplexStream) -> FramedLink<DuplexStream> {
    FramedLink::new(server)
}

/// # Panics
///
/// When a control message the golden holds does not survive the client's
/// decoding, or when fewer kinds than the golden holds are covered.
#[tokio::test]
async fn it_decodes_every_message_the_golden_holds() {
    let case = async {
        let mut seen = BTreeSet::new();
        for line in fixture()? {
            if line.get("direction").and_then(Value::as_str) != Some("to_client") {
                continue;
            }
            let (Some(hex), Some((named, _fields))) =
                (line.get("hex").and_then(Value::as_str), shaped(&line))
            else {
                continue;
            };
            // A `Delta` and a `Snapshot` carry payloads the message golden
            // keeps deliberately opaque; a client that accepted `00` as a
            // change would be wrong, so both are covered below with real ones.
            if named == "Delta" || named == "Snapshot" {
                continue;
            }
            let payload = golden::bytes(hex)?;
            let pair = pair();
            let mut client = pair.client;
            let mut server = framing(pair.server);
            server.send(CHANNEL_CONTROL, &payload).await?;
            let received = client.next(PROMPT).await?;
            let Received::Control(message) = received else {
                return Err(format!("{named}: not a control message").into());
            };
            assert_eq!(
                golden::hex(&encode_to_client(&message)?),
                golden::hex(&payload),
                "{named} did not survive decoding"
            );
            let _first = seen.insert(named);
        }
        let expected: BTreeSet<String> = [
            "Hello",
            "CommandResult",
            "PaneChannel",
            "PaneDetached",
            "Screen",
            "Mark",
            "Pong",
            "Error",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            seen, expected,
            "every kind the golden holds but the opaque two"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When pane bytes do not arrive with their channel, do not concatenate in
/// order under the pane they belong to, or when deltas are not kept in the
/// order they were received.
#[tokio::test]
async fn it_records_pane_bytes_and_deltas_in_order() {
    let case = async {
        let pair = pair();
        let mut client = pair.client;
        let mut server = framing(pair.server);
        let channel = 3;

        let announcement = ToClient::PaneChannel {
            pane: PANE,
            channel,
            sequence: Sequence(0),
        };
        server
            .send(CHANNEL_CONTROL, &encode_to_client(&announcement)?)
            .await?;
        let _announced = client.next(PROMPT).await?;

        for piece in [b"before ".as_slice(), b"and after".as_slice()] {
            server.send(channel, piece).await?;
            let received = client.next(PROMPT).await?;
            assert_eq!(
                received,
                Received::PaneBytes {
                    channel,
                    bytes: piece.to_vec()
                },
                "pane bytes arrive with the channel they came in on"
            );
        }
        assert_eq!(
            client.bytes_of(PANE),
            b"before and after",
            "and concatenate under the pane the channel carries"
        );

        let changes = [
            (
                Generation(4),
                Delta::PaneTitle {
                    pane: PANE,
                    title: "first".to_owned(),
                },
            ),
            (
                Generation(5),
                Delta::PaneTitle {
                    pane: PANE,
                    title: "second".to_owned(),
                },
            ),
        ];
        for (generation, delta) in changes.clone() {
            let message = ToClient::Delta {
                generation,
                payload: iznik_protocol::delta::encode_delta(&delta)?,
            };
            server
                .send(CHANNEL_CONTROL, &encode_to_client(&message)?)
                .await?;
            let _carried = client.next(PROMPT).await?;
        }
        assert_eq!(
            client.deltas(),
            changes,
            "deltas are kept in the order they came"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a client with nothing to receive waits past its deadline rather than
/// saying so.
#[tokio::test]
async fn it_refuses_to_wait_past_its_deadline() {
    let case = async {
        let pair = pair();
        let mut client = pair.client;
        let _held = pair.server;
        let started = Instant::now();
        let refused = client.next(BRIEF).await;
        let waited = started.elapsed();
        assert!(
            matches!(refused, Err(ClientError::Deadline { .. })),
            "a client with nothing to receive says so: {refused:?}"
        );
        assert!(
            waited < BRIEF.saturating_add(SLACK),
            "and says it in {BRIEF:?} and not {waited:?}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// Runs the handshake against a server that offers `offering`, and says what
/// the client made of it.
///
/// The server's reply goes out before the client's greeting is read: the pair
/// buffers both, so neither half has to be driven while the other waits.
///
/// # Errors
///
/// When either half of the handshake fails, or the client's first frame is
/// not the `Hello` it must be.
async fn shake(
    client: &mut TestClient<DuplexStream>,
    server: &mut FramedLink<DuplexStream>,
    offering: Capabilities,
) -> Result<ServerHello, Failed> {
    let reply = encode_to_client(&ToClient::Hello {
        protocol_version: PROTOCOL_VERSION,
        server_version: "scripted".to_owned(),
        capabilities: offering,
    })?;
    server.send(CHANNEL_CONTROL, &reply).await?;
    let shaken = client.hello(Capabilities::ZSTD).await?;
    let greeting = server
        .next_frame()
        .await?
        .ok_or("the client said nothing")?
        .payload
        .to_vec();
    let ToServer::Hello { capabilities, .. } = decode_to_server(&greeting)? else {
        return Err("the client's first frame was not a Hello".into());
    };
    if capabilities != Capabilities::ZSTD {
        return Err("the client did not offer what it was asked to".into());
    }
    Ok(shaken)
}

/// # Panics
///
/// When a client asked to keep up does not return credit for every byte or
/// acknowledge every detach, or when one that was not asked sends either.
#[tokio::test]
async fn it_returns_credit_and_acknowledges_detachment_when_asked() {
    let case = async {
        for returning in [true, false] {
            let pair = pair();
            let mut client = pair.client;
            let mut server = framing(pair.server);
            client.auto_credit(returning);
            let channel = 5;

            let announcement = ToClient::PaneChannel {
                pane: PANE,
                channel,
                sequence: Sequence(0),
            };
            server
                .send(CHANNEL_CONTROL, &encode_to_client(&announcement)?)
                .await?;
            let _announced = client.next(PROMPT).await?;
            server.send(channel, b"nine bytes").await?;
            let _carried = client.next(PROMPT).await?;
            let detachment = ToClient::PaneDetached {
                pane: PANE,
                channel,
            };
            server
                .send(CHANNEL_CONTROL, &encode_to_client(&detachment)?)
                .await?;
            let _detached = client.next(PROMPT).await?;

            let mut answers = Vec::new();
            while let Ok(Ok(Some(frame))) = tokio::time::timeout(BRIEF, server.next_frame()).await {
                answers.push(decode_to_server(frame.payload)?);
            }
            let expected = if returning {
                vec![
                    ToServer::Credit {
                        channel,
                        bytes: u32::try_from("nine bytes".len())?,
                    },
                    ToServer::ChannelReleased { channel },
                ]
            } else {
                Vec::new()
            };
            assert_eq!(answers, expected, "auto_credit({returning})");
        }
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When frames after a handshake in which both ends offered compression are
/// not compressed, or when frames after one in which the server did not are.
#[tokio::test]
async fn it_compresses_only_when_both_ends_advertise_it() {
    let case = async {
        let plain = encode_to_server(&ToServer::Ping)?;

        // Both offered it: a server that engages reads exactly what was sent.
        {
            let both = pair();
            let mut client = both.client;
            let mut server = framing(both.server);
            let greeting = shake(&mut client, &mut server, Capabilities::ZSTD).await?;
            assert_eq!(
                greeting.capabilities,
                Capabilities::ZSTD,
                "the server offered it"
            );
            let (stream, leftover) = server.into_parts();
            let mut engaged = compressed(stream, leftover)?;
            client.ping().await?;
            client.input(PANE, vec![b'x'; TYPED_BYTES]).await?;
            let ping = engaged
                .next_frame()
                .await?
                .ok_or("no ping")?
                .payload
                .to_vec();
            assert_eq!(decode_to_server(&ping)?, ToServer::Ping, "the ping decodes");
            let typed = engaged
                .next_frame()
                .await?
                .ok_or("no input")?
                .payload
                .to_vec();
            assert_eq!(
                decode_to_server(&typed)?,
                ToServer::Input {
                    pane: PANE,
                    bytes: vec![b'x'; TYPED_BYTES]
                },
                "and so do four kibibytes of input"
            );
        }

        // Both offered it, and what went on the wire is not the plain frame.
        {
            let wired = pair();
            let mut client = wired.client;
            let mut server = framing(wired.server);
            let _greeting = shake(&mut client, &mut server, Capabilities::ZSTD).await?;
            client.ping().await?;
            let (mut wire, _leftover) = server.into_parts();
            let seen = read_frame(&mut wire, &plain).await?;
            assert_ne!(
                golden::hex(&seen),
                golden::hex(&framed(&plain)?),
                "a compressed link does not put the plain frame on the wire"
            );
        }

        // The server did not offer it: the wire is exactly the plain frame.
        {
            let bare = pair();
            let mut client = bare.client;
            let mut server = framing(bare.server);
            let greeting = shake(&mut client, &mut server, Capabilities::RESUME).await?;
            assert_eq!(
                greeting.capabilities,
                Capabilities::RESUME,
                "and not compression"
            );
            client.ping().await?;
            let (mut wire, _leftover) = server.into_parts();
            let seen = read_frame(&mut wire, &plain).await?;
            assert_eq!(
                golden::hex(&seen),
                golden::hex(&framed(&plain)?),
                "a plain link puts the plain frame on the wire"
            );
        }
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
