//! A scripted host: it greets with a version and capabilities the case picks,
//! answers a command, and then comes back as a daemon with a model numbered
//! below the one it had.

use super::{AGAIN, ANSWERED, Failed, SETTLED, model_at};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, Created, encode_command_outcome};
use iznik_protocol::identity::Generation;
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use tokio::net::UnixListener;
use tokio::runtime::Runtime;

/// A host that answers a command and then comes back as a different daemon:
/// the same socket, a model numbered below the one it had.
///
/// # Errors
///
/// When the socket cannot be bound.
pub(super) fn starts_again(runtime: &Runtime, socket: &std::path::Path) -> Result<(), Failed> {
    serve(runtime, socket, "scripted", Capabilities::from_bits(0))
}

/// A scripted host answering `version` and `capabilities`, then behaving as a
/// daemon: a snapshot, an answer to a command, and a model that restarts.
///
/// # Errors
///
/// When the socket cannot be bound.
pub(super) fn serve(
    runtime: &Runtime,
    socket: &std::path::Path,
    version: &'static str,
    capabilities: Capabilities,
) -> Result<(), Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let _serving = runtime.spawn(async move {
        let Ok((stream, _from)) = listener.accept().await else {
            return;
        };
        let mut link = FramedLink::new(stream);
        loop {
            let heard = {
                let Ok(Some(frame)) = link.next_frame().await else {
                    return;
                };
                decode_to_server(frame.payload)
            };
            let said = match heard {
                Ok(ToServer::Hello { .. }) => encode_to_client(&ToClient::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    server_version: version.to_owned(),
                    capabilities,
                }),
                Ok(ToServer::SnapshotRequest) => model_at(SETTLED),
                // Answered, and then never announced: the window this client
                // holds a command applied in, which is where a daemon that
                // goes away leaves one for ever if nothing notices.
                Ok(ToServer::Command { command_id, .. }) => {
                    let Ok(payload) = encode_command_outcome(&CommandOutcome::Applied {
                        generation: Generation(ANSWERED),
                        created: Created::Nothing,
                    }) else {
                        return;
                    };
                    let answer = encode_to_client(&ToClient::CommandResult {
                        command_id,
                        payload,
                    });
                    let Ok(answer) = answer else {
                        return;
                    };
                    let _answered = link.send(CHANNEL_CONTROL, &answer).await;
                    // And now it is another daemon, with another model.
                    model_at(AGAIN)
                }
                Ok(_otherwise) => continue,
                Err(_unreadable) => return,
            };
            let Ok(said) = said else {
                return;
            };
            let _sent = link.send(CHANNEL_CONTROL, &said).await;
        }
    });
    Ok(())
}
