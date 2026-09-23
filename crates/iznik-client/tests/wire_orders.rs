//! What a host's task writes on the wire for what it is asked, heard by a
//! scripted host that writes down every message.
//!
//! The daemon is not what is under test, so none is started: the scripted
//! host answers the handshake and the snapshot, and what a case reads is the
//! exact sequence of messages this client sent it, connection by connection.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use iznik_client::host::manager::credit::CreditReceipt;
use iznik_client::host::manager::{HostManager, ManagerError, ManagerEvent, ManagerOptions};
use iznik_client::host::state::{BackoffPolicy, HostState};
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{PaneId, Sequence, SessionId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, MAXIMUM_INPUT_LENGTH, ToClient, ToServer, encode_to_client,
};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

#[path = "fixtures/listening_host.rs"]
mod listening_host;

use listening_host::{Heard, Script};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// The pane these cases type into.
const PANE: PaneId = PaneId(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A temporary directory of this case's own, removed when the guard drops.
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When it cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-wire-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the scripted host.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// A manager with quick timings and nothing to install.
///
/// # Errors
///
/// When the runtime paths cannot be made or the manager cannot be built.
fn manager(held: &Scratch) -> Result<HostManager, Failed> {
    let artifacts = held.path.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let mut options = ManagerOptions::new(artifacts, paths);
    options.backoff = BackoffPolicy {
        initial: Duration::from_millis(20),
        maximum: Duration::from_millis(200),
        ..BackoffPolicy::default()
    };
    options.channel = ChannelOptions {
        ping_interval: Duration::from_millis(50),
        pong_deadline: Duration::from_secs(2),
        open_deadline: Duration::from_secs(5),
        greeting_deadline: Duration::from_secs(2),
    };
    options.expire_interval = Duration::from_millis(50);
    Ok(HostManager::new(options)?)
}

/// A scripted host at a socket of the case's own, the alias that reaches it,
/// and what it hears.
///
/// # Errors
///
/// When the socket cannot be bound.
fn scripted(
    runtime: &Runtime,
    held: &Scratch,
    script: Script,
) -> Result<(String, Receiver<Heard>), Failed> {
    let socket = held.path.join("scripted.sock");
    let heard = listening_host::start(runtime, &socket, script)?;
    Ok((format!("{LOCAL_PREFIX}{}", socket.display()), heard))
}

/// Waits until the host is connected.
///
/// # Errors
///
/// When it is not inside [`PROMPT`].
fn await_connected(events: &Receiver<ManagerEvent>) -> Result<(), Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while let Some(left) = expires.checked_duration_since(Instant::now()) {
        if let Ok(ManagerEvent::Moved {
            state: HostState::Connected { .. },
            ..
        }) = events.recv_timeout(left)
        {
            return Ok(());
        }
    }
    Err("the host never connected".into())
}

/// Everything the scripted host heard, up to and including the first message
/// `last` accepts.
///
/// # Errors
///
/// When no such message arrives inside [`PROMPT`].
fn heard_until(
    heard: &Receiver<Heard>,
    last: impl Fn(&Heard) -> bool,
) -> Result<Vec<Heard>, Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut held = Vec::new();
    while let Some(left) = expires.checked_duration_since(Instant::now()) {
        let Ok(one) = heard.recv_timeout(left) else {
            break;
        };
        let done = last(&one);
        held.push(one);
        if done {
            return Ok(held);
        }
    }
    Err(format!(
        "what was waited for never came; heard {} messages",
        held.len()
    )
    .into())
}

/// Whether a message is keystrokes that are exactly `bytes`.
fn typed(message: &ToServer, bytes: &[u8]) -> bool {
    matches!(message, ToServer::Input { bytes: said, .. } if said == bytes)
}

/// # Panics
///
/// When a paste larger than one message can carry tears down the link, or
/// arrives other than whole and in order on the connection it was given to.
#[test]
fn wire_orders_carry_a_paste_larger_than_a_message() {
    let case = || -> Result<(), Failed> {
        let held = scratch("paste")?;
        let runtime = runtime()?;
        let (host, heard) = scripted(&runtime, &held, Script::default())?;
        let manager = manager(&held)?;
        let events = manager.events();
        manager.add_host(&host)?;
        await_connected(&events)?;
        let most = usize::try_from(MAXIMUM_INPUT_LENGTH)?;
        let paste: Vec<u8> = (0..most.saturating_mul(3).saturating_add(most / 2))
            .map(|index| u8::try_from(index % 251).unwrap_or(0))
            .collect();
        manager.input(&host, PANE, paste.clone())?;
        manager.input(&host, PANE, b"after".to_vec())?;
        let said = heard_until(&heard, |(_, message)| typed(message, b"after"))?;
        let mut carried = Vec::new();
        let mut pieces = 0_usize;
        for (connection, message) in &said {
            if let ToServer::Input { pane, bytes } = message
                && !typed(message, b"after")
            {
                assert_eq!(
                    *connection, 0,
                    "the link that took the paste is the one it began on"
                );
                assert_eq!(*pane, PANE, "every piece is for the pane it was typed into");
                assert!(bytes.len() <= most, "and fits one message: {}", bytes.len());
                carried.extend_from_slice(bytes);
                pieces = pieces.saturating_add(1);
            }
        }
        assert!(pieces > 1, "the paste was cut into pieces: {pieces}");
        assert!(carried == paste, "and arrives whole and in order");
        assert!(
            said.iter().all(|(connection, _)| *connection == 0),
            "without the link being dropped and made again"
        );
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a command too large to send is accepted, or refusing it disturbs the
/// link.
#[test]
fn wire_orders_refuse_a_command_too_large_to_send() {
    let case = || -> Result<(), Failed> {
        let held = scratch("oversize")?;
        let runtime = runtime()?;
        let (host, heard) = scripted(&runtime, &held, Script::default())?;
        let manager = manager(&held)?;
        let events = manager.events();
        manager.add_host(&host)?;
        await_connected(&events)?;
        let name = "n".repeat(usize::try_from(MAXIMUM_INPUT_LENGTH)?.saturating_add(1));
        let refused = manager.command(
            &host,
            SessionCommand::RenameSession {
                session: SessionId(1),
                name,
            },
        );
        assert!(
            matches!(refused, Err(ManagerError::Oversize { .. })),
            "a command too large for a message is refused at once: {refused:?}"
        );
        manager.input(&host, PANE, b"still".to_vec())?;
        let said = heard_until(&heard, |(_, message)| typed(message, b"still"))?;
        assert!(
            said.iter().all(|(connection, _)| *connection == 0),
            "and the link carries on"
        );
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// The channel the scripted host carries its pane on.
const PANE_CHANNEL: u8 = 7;

/// What the scripted host prints, a frame at a time.
const PRINTED: [&[u8]; 5] = [b"one ", b"two ", b"three ", b"four ", b"five"];

/// A script that, when the pane is subscribed to, announces its channel and
/// prints [`PRINTED`] on it.
///
/// # Errors
///
/// When the announcement cannot be encoded.
fn printing() -> Result<Script, Failed> {
    let announced = encode_to_client(&ToClient::PaneChannel {
        pane: PANE,
        channel: PANE_CHANNEL,
        sequence: Sequence(0),
    })?;
    let mut on_subscribe = vec![(CHANNEL_CONTROL, announced)];
    on_subscribe.extend(PRINTED.iter().map(|bytes| (PANE_CHANNEL, bytes.to_vec())));
    Ok(Script {
        on_subscribe,
        ..Script::default()
    })
}

/// The receipts of the first `count` deliveries of a pane's bytes.
///
/// # Errors
///
/// When they do not all arrive inside [`PROMPT`].
fn receipts(events: &Receiver<ManagerEvent>, count: usize) -> Result<Vec<CreditReceipt>, Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut held = Vec::new();
    while held.len() < count {
        let left = expires
            .checked_duration_since(Instant::now())
            .ok_or("the pane's bytes never all arrived")?;
        if let Ok(ManagerEvent::Bytes {
            receipt: Some(receipt),
            ..
        }) = events.recv_timeout(left)
        {
            held.push(receipt);
        }
    }
    Ok(held)
}

/// # Panics
///
/// When credit returned a delivery at a time goes back as a message per
/// delivery rather than one per stream for the turn, or as other than exactly
/// what was delivered.
#[test]
fn wire_orders_return_a_turn_of_credit_as_one_message() {
    let case = || -> Result<(), Failed> {
        let held = scratch("credit")?;
        let runtime = runtime()?;
        let (host, heard) = scripted(&runtime, &held, printing()?)?;
        let manager = manager(&held)?;
        let events = manager.events();
        manager.add_host(&host)?;
        await_connected(&events)?;
        manager.subscribe(&host, PANE)?;
        let delivered = receipts(&events, PRINTED.len())?;
        // Something slow for the task to write first, so that every credit
        // is already waiting when it gets to them: the turn is what they
        // share, and this is what puts them in one.
        let paste = vec![b'x'; usize::try_from(MAXIMUM_INPUT_LENGTH)?.saturating_mul(2)];
        manager.input(&host, PANE, paste)?;
        for receipt in &delivered {
            manager.credit_receipt(receipt)?;
        }
        manager.input(&host, PANE, b"done".to_vec())?;
        let said = heard_until(&heard, |(_, message)| typed(message, b"done"))?;
        let credited: Vec<(u8, u32)> = said
            .iter()
            .filter_map(|(_, message)| match message {
                ToServer::Credit { channel, bytes } => Some((*channel, *bytes)),
                _otherwise => None,
            })
            .collect();
        let total: usize = PRINTED.iter().map(|bytes| bytes.len()).sum();
        assert_eq!(
            credited,
            vec![(PANE_CHANNEL, u32::try_from(total)?)],
            "one message for the stream, carrying exactly what was delivered"
        );
        drop(manager);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
