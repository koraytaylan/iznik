//! `iznik tail <host> <pane>`: a pane's bytes as they arrive, until a signal.
//!
//! The one command here that does not end on its own. It prints what a pane
//! says as it says it — one object a line, the bytes as base 64 because a
//! pane's output is bytes and half a character may arrive before the other
//! half — and stops when it is interrupted, saying that it did rather than
//! dying of the signal.
//!
//! The first line is the pane as it stands, because that is what a host sends
//! a client that has just attached and it is what the bytes after it are
//! changes to: a reader that skipped it would be reading the middle of
//! something. It is marked as what it is, so a script can tell the two
//! apart.

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_protocol::identity::PaneId;

use crate::output::{Value, bytes, line, object, refusal, text};
use crate::{CLIENT_LAYER, TRANSPORT_LAYER, USAGE_EXIT_CODE, holding, runtime};

/// What this subcommand takes.
const USAGE: &str = "usage: iznik tail <host> <pane>";

/// How often the loop looks for the signal when nothing is arriving.
const LOOK: std::time::Duration = std::time::Duration::from_millis(100);

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some((alias, pane)) = a_host_and_a_pane(arguments) else {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, USAGE);
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    let told = Arc::clone(&interrupted);
    let waiting = match runtime() {
        Ok(built) => built,
        Err(source) => {
            let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, &source.to_string());
            return ExitCode::FAILURE;
        }
    };
    // Listened for before anything that can take minutes: reaching a host may
    // mean installing a server on it, and an interruption during that must be
    // this command ending rather than this command being killed.
    let _watching = waiting.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            told.store(true, Ordering::Release);
        }
    });
    let (manager, events) = match holding(&alias, &interrupted) {
        Ok(reached) => reached,
        Err((layer, detail)) => {
            // Interrupted on the way is an ending, not a failure: nothing was
            // asked of the host that anybody is waiting to hear about.
            if interrupted.load(Ordering::Acquire) {
                return ExitCode::SUCCESS;
            }
            let _said = refusal(&mut std::io::stderr(), layer, &detail);
            return ExitCode::FAILURE;
        }
    };
    if let Err(refused) = manager.subscribe(&alias, pane) {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, &refused.to_string());
        return ExitCode::FAILURE;
    }
    // Credit is the application's to return, and this application consumes
    // everything the moment it arrives.
    while !interrupted.load(Ordering::Acquire) {
        let Ok(event) = events.recv_timeout(LOOK) else {
            continue;
        };
        let (owed, printed) = match event {
            ManagerEvent::Screen {
                pane: named,
                sequence,
                columns,
                rows,
                bytes: said,
                ..
            } if named == pane => {
                // No credit: a screen comes on the control channel and spends
                // none of the pane's window, so returning any would hand the
                // host room this reader has not made.
                (0, drawn(pane, sequence.0, columns, rows, &said))
            }
            ManagerEvent::Bytes {
                pane: named,
                sequence,
                bytes: said,
                ..
            } if named == pane => (said.len(), shaped(pane, sequence.0, &said)),
            // A pane the host does not have is refused, and a refusal thrown
            // away here would leave a script unable to tell a pane that does
            // not exist from a pane that is quiet.
            ManagerEvent::Notify(Notification::Refused { code, message, .. }) => {
                let _said = refusal(
                    &mut std::io::stderr(),
                    TRANSPORT_LAYER,
                    &format!("{code:?}: {message}"),
                );
                return ExitCode::FAILURE;
            }
            // And a pane that has gone is an ending, said rather than waited
            // out: nothing more will ever arrive for it.
            ManagerEvent::Detached { pane: named, .. } if named == pane => {
                let _printed = line(&mut std::io::stdout(), &detached(pane));
                return ExitCode::SUCCESS;
            }
            _elsewhere => continue,
        };
        if line(&mut std::io::stdout(), &printed).is_err() {
            // Whoever was reading has gone, which is an ending too.
            break;
        }
        if owed > 0 {
            let taken = u32::try_from(owed).unwrap_or(u32::MAX);
            let _returned = manager.credit(&alias, pane, taken);
        }
    }
    ExitCode::SUCCESS
}

/// One delivery of a pane's bytes, as one object.
fn shaped(pane: PaneId, sequence: u64, said: &[u8]) -> Value {
    object(vec![
        ("kind", text("output")),
        ("pane", Value::Whole(pane.0)),
        ("sequence", Value::Whole(sequence)),
        ("bytes", bytes(said)),
        ("encoding", text("base64")),
    ])
}

/// A pane that has gone, as one object.
fn detached(pane: PaneId) -> Value {
    object(vec![
        ("kind", text("detached")),
        ("pane", Value::Whole(pane.0)),
    ])
}

/// The pane as it stands, as one object.
///
/// With the size it was drawn at, for the same reason the boundary carries it
/// there: what these bytes reproduce is a screen of that shape, and a reader
/// that assumed another would put every one of them in the wrong place.
fn drawn(pane: PaneId, sequence: u64, columns: u16, rows: u16, said: &[u8]) -> Value {
    object(vec![
        ("kind", text("screen")),
        ("pane", Value::Whole(pane.0)),
        ("sequence", Value::Whole(sequence)),
        ("columns", Value::Whole(u64::from(columns))),
        ("rows", Value::Whole(u64::from(rows))),
        ("bytes", bytes(said)),
        ("encoding", text("base64")),
    ])
}

/// The host and the pane this subcommand takes, or nothing.
fn a_host_and_a_pane(arguments: &[OsString]) -> Option<(String, PaneId)> {
    let mut rest = arguments.iter().skip(1);
    let alias = rest.next()?.to_str()?.to_owned();
    let pane = rest.next()?.to_str()?.parse::<u64>().ok()?;
    rest.next().is_none().then_some((alias, PaneId(pane)))
}
