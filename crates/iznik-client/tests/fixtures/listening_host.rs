//! A scripted host that writes down everything a client says to it.
//!
//! It shakes hands, answers every request for its model with an empty one,
//! answers pings, sends whatever frames a case scripted after the first
//! snapshot of each connection, and closes the connection when told to — accepting the next
//! connection afterwards. What a case reads is every other message, in the
//! order it arrived, with the connection it arrived on.

use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::identity::Generation;
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_server, encode_to_client,
};
use iznik_protocol::model::{HostModel, encode_host_model};
use std::sync::mpsc::{Receiver, Sender, channel};
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::Runtime;

use super::Failed;

/// The generation the scripted host's model is at.
const SETTLED: u64 = 1;

/// What the scripted host does besides answering.
#[derive(Clone, Debug, Default)]
pub(super) struct Script {
    /// Frames sent on each connection once its first snapshot has been
    /// answered: the channel, and the payload.
    pub after_snapshot: Vec<(u8, Vec<u8>)>,
    /// Frames sent whenever a pane is subscribed to.
    pub on_subscribe: Vec<(u8, Vec<u8>)>,
    /// What every command is answered with, as the payload of its
    /// `CommandResult`, when commands are answered at all.
    pub answer_commands_with: Option<Vec<u8>>,
    /// What the host says its server is, when not `scripted`.
    pub version: Option<String>,
    /// Keystrokes that end the connection they arrive on.
    pub close_on: Option<Vec<u8>>,
}

/// One message heard: the connection it arrived on, counted from zero, and
/// the message.
pub(super) type Heard = (usize, ToServer);

/// Binds the scripted host at `socket` and gives back what it hears.
///
/// # Errors
///
/// When the socket cannot be bound.
pub(super) fn start(
    runtime: &Runtime,
    socket: &std::path::Path,
    script: Script,
) -> Result<Receiver<Heard>, Failed> {
    let listener = runtime.block_on(async { UnixListener::bind(socket) })?;
    let (telling, heard) = channel();
    let _serving = runtime.spawn(async move {
        let mut connection = 0_usize;
        while let Ok((stream, _from)) = listener.accept().await {
            let serving = Serving {
                connection,
                script: script.clone(),
                telling: telling.clone(),
            };
            // A connection that ended badly — the client dropped it while it
            // was being written to — is one connection, and the next is
            // still taken.
            let _ended = serving.serve(FramedLink::new(stream)).await;
            connection = connection.saturating_add(1);
        }
    });
    Ok(heard)
}

/// One connection's worth of the script.
struct Serving {
    /// Which connection it is.
    connection: usize,
    /// What to do.
    script: Script,
    /// Where what is heard goes.
    telling: Sender<Heard>,
}

impl Serving {
    /// Answers one connection until it ends, one way or the other.
    ///
    /// # Errors
    ///
    /// When nobody is listening for what it hears any more.
    async fn serve(self, mut link: FramedLink<UnixStream>) -> Result<(), String> {
        let mut answered = false;
        loop {
            let heard = {
                let Ok(Some(frame)) = link.next_frame().await else {
                    return Ok(());
                };
                if frame.channel != CHANNEL_CONTROL {
                    continue;
                }
                decode_to_server(frame.payload).map_err(|error| error.to_string())?
            };
            match &heard {
                ToServer::Hello { .. } => {
                    let greeting = ToClient::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        server_version: self
                            .script
                            .version
                            .clone()
                            .unwrap_or_else(|| "scripted".to_owned()),
                        capabilities: Capabilities::from_bits(0),
                    };
                    send(&mut link, &greeting).await?;
                    continue;
                }
                ToServer::Ping => {
                    send(&mut link, &ToClient::Pong).await?;
                    continue;
                }
                ToServer::SnapshotRequest => {
                    let payload = encode_host_model(&HostModel {
                        generation: Generation(SETTLED),
                        sessions: Vec::new(),
                    })
                    .map_err(|error| error.to_string())?;
                    let snapshot = ToClient::Snapshot {
                        generation: Generation(SETTLED),
                        payload,
                    };
                    send(&mut link, &snapshot).await?;
                    if !answered {
                        answered = true;
                        say(&mut link, &self.script.after_snapshot).await?;
                    }
                }
                ToServer::Subscribe { .. } => say(&mut link, &self.script.on_subscribe).await?,
                ToServer::Command { command_id, .. } => {
                    if let Some(payload) = &self.script.answer_commands_with {
                        let answer = ToClient::CommandResult {
                            command_id: *command_id,
                            payload: payload.clone(),
                        };
                        send(&mut link, &answer).await?;
                    }
                }
                _otherwise => {}
            }
            let closing = matches!(
                (&heard, &self.script.close_on),
                (ToServer::Input { bytes, .. }, Some(ending)) if bytes == ending
            );
            self.telling
                .send((self.connection, heard))
                .map_err(|error| error.to_string())?;
            if closing {
                return Ok(());
            }
        }
    }
}

/// Sends scripted frames, each on its own channel.
///
/// # Errors
///
/// When the link will not take one.
async fn say(link: &mut FramedLink<UnixStream>, frames: &[(u8, Vec<u8>)]) -> Result<(), String> {
    for (number, bytes) in frames {
        link.send(*number, bytes)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Sends one control message.
///
/// # Errors
///
/// When it cannot be encoded or the link will not take it.
pub(super) async fn send(
    link: &mut FramedLink<UnixStream>,
    message: &ToClient,
) -> Result<(), String> {
    let bytes = encode_to_client(message).map_err(|error| error.to_string())?;
    link.send(CHANNEL_CONTROL, &bytes)
        .await
        .map_err(|error| error.to_string())
}
