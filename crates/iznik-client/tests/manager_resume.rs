//! A pane let go of and asked for again from the byte its reader stands at is
//! continued from there, not drawn again from a fresh screen.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use iznik_client::host::manager::{HostManager, ManagerEvent, ManagerOptions};
use iznik_client::host::state::HostState;
use iznik_client::reduce::Notification;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::Builder as RuntimeBuilder;

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// The pane the session comes with.
const PANE: PaneId = PaneId(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// Waits for an event the predicate accepts, and gives it back with every
/// event that went past before it.
///
/// # Errors
///
/// When none arrives inside [`PROMPT`].
fn await_event(
    events: &Receiver<ManagerEvent>,
    what: &str,
    wanted: impl Fn(&ManagerEvent) -> bool,
) -> Result<(ManagerEvent, Vec<ManagerEvent>), Failed> {
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut passed = Vec::new();
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(event) if wanted(&event) => return Ok((event, passed)),
            Ok(other) => passed.push(other),
            Err(_nothing) => break,
        }
    }
    Err(format!("no {what} inside {PROMPT:?}; what came was {passed:?}").into())
}

/// Reads the pane's bytes until `needle` has been said, and gives back the
/// byte after the last one read.
///
/// # Errors
///
/// When it is not said inside [`PROMPT`].
fn read_until(events: &Receiver<ManagerEvent>, needle: &[u8]) -> Result<Sequence, Failed> {
    let mut said = Vec::new();
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        if let Ok(ManagerEvent::Bytes {
            sequence, bytes, ..
        }) = events.recv_timeout(left)
        {
            said.extend_from_slice(&bytes);
            if said.windows(needle.len()).any(|window| window == needle) {
                return Ok(Sequence(
                    sequence.0.saturating_add(u64::try_from(bytes.len())?),
                ));
            }
        }
    }
    Err(format!("the pane never said {:?}", String::from_utf8_lossy(needle)).into())
}

/// # Panics
///
/// When a resumed pane is sent a screen, or continues from anywhere but the
/// byte it was asked to continue from.
#[test]
fn manager_resume_continues_a_pane_from_the_byte_it_holds() {
    let case = || -> Result<(), Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let held = base.join(format!("iznik-manager-resume-{}", std::process::id()));
        let _stale = std::fs::remove_dir_all(&held);
        std::fs::create_dir_all(held.join("artifacts"))?;
        let runtime = RuntimeBuilder::new_multi_thread().enable_all().build()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let manager = HostManager::new(ManagerOptions::new(
            held.join("artifacts"),
            ClientRuntimePaths::under(&held.join("runtime"))?,
        ))?;
        let events = manager.events();
        let host = format!("{LOCAL_PREFIX}{}", stack.socket().display());
        manager.add_host(&host)?;
        let _connected = await_event(&events, "a connection", |event| {
            matches!(
                event,
                ManagerEvent::Moved {
                    state: HostState::Connected { .. },
                    ..
                }
            )
        })?;
        let _made = manager.command(
            &host,
            SessionCommand::CreateSession {
                name: "work".to_owned(),
                columns: 80,
                rows: 24,
                working_directory: None,
            },
        )?;
        let _answered = await_event(&events, "the session", |event| {
            matches!(
                event,
                ManagerEvent::Notify(Notification::CommandFinished { .. })
            )
        })?;
        manager.subscribe(&host, PANE)?;
        let _carried = await_event(
            &events,
            "the pane carried",
            |event| matches!(event, ManagerEvent::Carried { pane, .. } if *pane == PANE),
        )?;
        manager.input(&host, PANE, b"echo first-$((40+2))\n".to_vec())?;
        let held_at = read_until(&events, b"first-42")?;
        manager.unsubscribe(&host, PANE)?;
        let _let_go = await_event(
            &events,
            "the pane let go",
            |event| matches!(event, ManagerEvent::Detached { pane, .. } if *pane == PANE),
        )?;
        manager.resume(&host, PANE, held_at)?;
        let (carried, before) = await_event(
            &events,
            "the pane carried again",
            |event| matches!(event, ManagerEvent::Carried { pane, .. } if *pane == PANE),
        )?;
        assert!(
            matches!(carried, ManagerEvent::Carried { sequence, .. } if sequence == held_at),
            "continued from the byte asked for: {carried:?}"
        );
        manager.input(&host, PANE, b"echo second-$((40+2))\n".to_vec())?;
        let (first, between) = await_event(&events, "bytes after the resume", |event| {
            matches!(
                event,
                ManagerEvent::Bytes { .. } | ManagerEvent::Screen { .. }
            )
        })?;
        assert!(
            matches!(first, ManagerEvent::Bytes { sequence, .. } if sequence == held_at),
            "the first thing carried is the byte after what was held, not a screen: \
             {first:?} after {before:?} and {between:?}"
        );
        drop(manager);
        drop(stack);
        let _gone = std::fs::remove_dir_all(&held);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
