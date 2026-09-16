//! Scripted framed peer for delivery receipts; no daemon, shell or SSH environment.

use super::{Failed, PANE, SETTLED, model_at};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use std::sync::mpsc::{Receiver, channel};
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::Runtime;

/// Original wire channel, deliberately distinct from its replacement.
const ORIGINAL_CHANNEL: u8 = 7;
/// Replacement wire channel whose credit the test observes.
pub(super) const REPLACEMENT_CHANNEL: u8 = 8;
/// Independent pane whose stream must survive its sibling's replacement.
pub(super) const OTHER_PANE: PaneId = PaneId(2);
/// Separate channel, never involved in the replacement under test.
pub(super) const OTHER_CHANNEL: u8 = 9;
/// Independent output held across the sibling replacement.
pub(super) const OTHER_BYTES: &[u8] = b"other";
/// A delivery held by the UI across channel replacement.
pub(super) const ORIGINAL_BYTES: &[u8] = b"old";
/// A current delivery eligible for exactly one credit return.
pub(super) const CURRENT_BYTES: &[u8] = b"current";
/// Script control carried through the ordinary input path.
pub(super) const REPLACE: &[u8] = b"replace";
/// Wire-order barrier after all submitted credit orders.
pub(super) const FINISH: &[u8] = b"finish";
/// Every credit frame actually observed by the scripted peer, in wire order.
pub(super) type Grants = Vec<(u8, u32)>;

/// Bind an isolated local peer and report its full wire history after the input barrier.
///
/// # Errors
/// Returns a socket binding failure.
pub(super) fn start(
    runtime: &Runtime,
    socket: &std::path::Path,
) -> Result<Receiver<Result<Grants, String>>, Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let (sender, receiver) = channel();
    let _serving = runtime.spawn(async move {
        let result = serve(listener).await;
        let _reported = sender.send(result);
    });
    Ok(receiver)
}

/// Run the script until the caller's wire-order barrier, retaining every credit frame.
///
/// # Errors
/// Returns a closed peer or failed codec/socket operation.
async fn serve(listener: UnixListener) -> Result<Grants, String> {
    let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
    let mut link = FramedLink::new(stream);
    let mut grants = Vec::new();
    loop {
        let request = {
            let frame = link
                .next_frame()
                .await
                .map_err(|error| error.to_string())?
                .ok_or("peer closed")?;
            decode_to_server(frame.payload).map_err(|error| error.to_string())?
        };
        match request {
            ToServer::Hello { .. } => {
                send(
                    &mut link,
                    &ToClient::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: "scripted".to_owned(),
                        capabilities: Capabilities::from_bits(0),
                    },
                )
                .await?;
            }
            ToServer::SnapshotRequest => {
                let bytes = model_at(SETTLED).map_err(|error| error.to_string())?;
                link.send(CHANNEL_CONTROL, &bytes)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            ToServer::Subscribe { pane } => {
                let (channel, bytes) = if pane == OTHER_PANE {
                    (OTHER_CHANNEL, OTHER_BYTES)
                } else {
                    (ORIGINAL_CHANNEL, ORIGINAL_BYTES)
                };
                deliver(&mut link, pane, channel, Sequence(0), bytes).await?;
            }
            ToServer::Input { bytes, .. } if bytes == REPLACE => {
                let sequence = Sequence(
                    u64::try_from(ORIGINAL_BYTES.len()).map_err(|error| error.to_string())?,
                );
                deliver(
                    &mut link,
                    PANE,
                    REPLACEMENT_CHANNEL,
                    sequence,
                    CURRENT_BYTES,
                )
                .await?;
            }
            ToServer::Input { bytes, .. } if bytes == FINISH => return Ok(grants),
            ToServer::Credit { channel, bytes } => grants.push((channel, bytes)),
            ToServer::Ping => send(&mut link, &ToClient::Pong).await?,
            _ => {}
        }
    }
}

/// Announce a stream before its bytes, preserving real control/data ordering.
///
/// # Errors
/// Returns a codec or socket failure.
async fn deliver(
    link: &mut FramedLink<UnixStream>,
    pane: PaneId,
    channel: u8,
    sequence: Sequence,
    bytes: &[u8],
) -> Result<(), String> {
    send(
        link,
        &ToClient::PaneChannel {
            pane,
            channel,
            sequence,
        },
    )
    .await?;
    link.send(channel, bytes)
        .await
        .map_err(|error| error.to_string())
}

/// Use the production protocol codec and framed link for every scripted control message.
///
/// # Errors
/// Returns a codec or socket failure.
async fn send(link: &mut FramedLink<UnixStream>, message: &ToClient) -> Result<(), String> {
    let bytes = encode_to_client(message).map_err(|error| error.to_string())?;
    link.send(CHANNEL_CONTROL, &bytes)
        .await
        .map_err(|error| error.to_string())
}
