//! Cancel a receive at its cooperative boundary without losing a buffered frame.

#[path = "fixtures/channel_cancellation.rs"]
mod fixture;

use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use iznik_client::transport::channel::{ChannelOptions, RemoteChannel};
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX, Transport};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::frame::FrameHeader;
use iznik_protocol::message::{CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, encode_to_client};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::task::coop::{RestoreOnPending, poll_proceed};

/// Entire local scheduling proof deadline; no step can hold the test open indefinitely.
const DEADLINE: Duration = Duration::from_secs(2);
/// Guard against an unconstrained runtime; comfortably above the pinned runtime's budget.
const MAXIMUM_BUDGET_PROBES: usize = 1_024;
/// Errors cross the test boundary as one assertion with their original detail.
type Failed = Box<dyn std::error::Error>;

/// Private socket paths disappear even when the proof fails.
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// Leave exactly one cooperative operation, without depending on the budget's numeric size.
///
/// # Errors
/// Returns an unconstrained runtime or a budget already empty on entry.
fn one_operation(context: &mut Context<'_>) -> Result<(), Failed> {
    let mut restore: Option<RestoreOnPending> = None;
    for _ in 0..MAXIMUM_BUDGET_PROBES {
        match poll_proceed(context) {
            Poll::Ready(next) => {
                if let Some(previous) = restore.replace(next) {
                    previous.made_progress();
                }
            }
            Poll::Pending => {
                return restore
                    .map(drop)
                    .ok_or_else(|| "budget already empty".into());
            }
        }
    }
    Err("runtime did not constrain cooperative work".into())
}

/// Encode the greeting and all pane frames in one small peer write.
///
/// # Errors
/// Returns protocol encoding or frame-length failure.
fn frames() -> Result<Vec<u8>, Failed> {
    let hello = encode_to_client(&ToClient::Hello {
        protocol_version: PROTOCOL_VERSION,
        server_version: "cancellation-fixture".to_owned(),
        capabilities: Capabilities::from_bits(0),
    })?;
    let mut bytes = Vec::new();
    for (channel, payload) in std::iter::once((CHANNEL_CONTROL, hello.as_slice())).chain(
        fixture::EXPECTED
            .iter()
            .map(|payload| (fixture::CHANNEL, *payload)),
    ) {
        FrameHeader::for_payload(channel, payload)?.write(&mut bytes);
        bytes.extend_from_slice(payload);
    }
    Ok(bytes)
}

/// A bounded proof that consumes buffered frames while the manager's cancellation is possible.
///
/// # Errors
/// Returns fixture setup, channel or frame-sequence failures.
async fn run() -> Result<(), Failed> {
    let directory = Directory(
        std::env::temp_dir().join(format!("iznik-channel-cancellation-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0)?;
    let socket = directory.0.join("channel.sock");
    let listener = UnixListener::bind(&socket)?;
    let bytes = frames()?;
    let (finish, finished) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (incoming, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let mut link = FramedLink::new(incoming);
        link.next_frame()
            .await
            .map_err(|error| error.to_string())?
            .ok_or("client closed before hello")?;
        let (mut stream, _) = link.into_parts();
        stream
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        let _finished = finished.await;
        Ok::<(), String>(())
    });
    let paths = ClientRuntimePaths::under(&directory.0.join("runtime"))?;
    let transport = Transport::for_alias(
        &format!("{LOCAL_PREFIX}{}", socket.display()),
        &paths,
        SshOptions::default(),
    );
    let mut channel = RemoteChannel::open(&transport, None, ChannelOptions::default()).await?;
    let deadline = Instant::now()
        .checked_add(DEADLINE)
        .ok_or("deadline overflow")?;
    let first = channel.next(deadline).await?;
    tokio::task::yield_now().await;
    // Dropping a pending future is precisely what a winning outgoing order does.
    let attempt = poll_fn(|context| {
        if let Err(error) = one_operation(context) {
            return Poll::Ready(Err(error));
        }
        let mut receiving = std::pin::pin!(channel.next(deadline));
        Poll::Ready(Ok(match receiving.as_mut().poll(context) {
            Poll::Ready(received) => Some(received),
            Poll::Pending => None,
        }))
    })
    .await?;
    tokio::task::yield_now().await;
    let second = match attempt {
        Some(received) => received?,
        None => channel.next(deadline).await?,
    };
    if second.payload != fixture::SECOND {
        return Err(format!(
            "cancelled receive lost the middle frame: {:?}",
            second.payload
        )
        .into());
    }
    let third = channel.next(deadline).await?;
    let observed = [
        first.payload.as_slice(),
        second.payload.as_slice(),
        third.payload.as_slice(),
    ];
    if observed != fixture::EXPECTED {
        return Err(format!("received {observed:?}, expected {:?}", fixture::EXPECTED).into());
    }
    let _finished = finish.send(());
    peer.await?.map_err(|error| -> Failed { error.into() })?;
    Ok(())
}

/// # Panics
/// Fails when cancellation discards a frame or any fixture step exceeds its deadline.
#[tokio::test]
async fn channel_cancellation_preserves_every_buffered_frame() {
    let result = tokio::time::timeout(DEADLINE, run()).await;
    assert!(
        matches!(&result, Ok(Ok(()))),
        "cancellation proof: {result:?}"
    );
}
