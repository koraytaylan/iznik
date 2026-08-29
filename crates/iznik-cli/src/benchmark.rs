//! `iznik benchmark <host>`: the keystroke round trip against a host, printed
//! as a distribution.
//!
//! The number that decides whether this is usable: how long a key takes to
//! come back. Not an average — an average hides the one keystroke in a
//! hundred that took a second, which is the one a person notices — so what is
//! printed is the shape of it, from the quickest to the slowest with the
//! middle and the ninety-ninth between them.

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{HostManager, ManagerEvent};
use iznik_client::reduce::Notification;
use iznik_protocol::command::{CommandOutcome, Created, SessionCommand};
use iznik_protocol::identity::{PaneId, SessionId};

use crate::output::{Value, line, object, refusal, text};
use crate::{CLIENT_LAYER, TRANSPORT_LAYER, USAGE_EXIT_CODE, holding, one_host};

/// What this subcommand takes.
const USAGE: &str = "usage: iznik benchmark <host>";

/// How many keystrokes are timed.
const KEYSTROKES: usize = 100;

/// The width the pane it makes is made at.
const COLUMNS: u16 = 80;

/// And its height.
const ROWS: u16 = 24;

/// How long one keystroke may take before the run gives up on it.
const KEYSTROKE_DEADLINE: Duration = Duration::from_secs(10);

/// How long the pane it asks for may take to exist.
const PANE_DEADLINE: Duration = Duration::from_secs(30);

/// What is typed: a character a shell in its default state echoes and does
/// nothing else about.
const KEYSTROKE: &[u8] = b"x";

/// Where in the sorted times the middle is, as a fraction of the way along.
const MEDIAN: (usize, usize) = (1, 2);

/// And the ninety-ninth percentile.
const NINETY_NINTH: (usize, usize) = (99, 100);

/// And the two ends.
const QUICKEST: (usize, usize) = (0, 1);

/// The slowest of them, which is the one somebody remembers.
const SLOWEST: (usize, usize) = (1, 1);

/// How many milliseconds are in a second, as a divisor for a duration.
const MILLISECONDS: f64 = 1000.0;

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    let Some(alias) = one_host(arguments) else {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, USAGE);
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    match timed(&alias) {
        Ok(taken) => {
            let _printed = line(&mut std::io::stdout(), &shaped(&alias, &taken));
            ExitCode::SUCCESS
        }
        Err((layer, detail)) => {
            let _said = refusal(&mut std::io::stderr(), layer, &detail);
            ExitCode::FAILURE
        }
    }
}

/// Times the round trip, and says which layer refused when one did.
///
/// # Errors
///
/// The layer and what it said.
fn timed(alias: &str) -> Result<Vec<Duration>, (&'static str, String)> {
    let uninterrupted = AtomicBool::new(false);
    let (manager, events) = holding(alias, &uninterrupted)?;
    measured(&manager, &events, alias, KEYSTROKES)
}

/// Times `keystrokes` round trips against a host already held.
///
/// Its own session, made and closed here, because what is typed appears on
/// the screen it is typed into and a measurement has no business writing on
/// somebody's work.
///
/// # Errors
///
/// The layer and what it said.
pub(crate) fn measured(
    manager: &HostManager,
    events: &std::sync::mpsc::Receiver<ManagerEvent>,
    alias: &str,
    keystrokes: usize,
) -> Result<Vec<Duration>, (&'static str, String)> {
    let (session, pane) = a_pane_of_its_own(manager, events, alias)?;
    manager
        .subscribe(alias, pane)
        .map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let mut taken = Vec::with_capacity(keystrokes);
    for _keystroke in 0..keystrokes {
        // Everything the pane has already said, taken and paid for, so that
        // what is timed below is what this keystroke caused and not what the
        // one before it left behind.
        settled(manager, events, alias, pane);
        let began = Instant::now();
        manager
            .input(alias, pane, KEYSTROKE.to_vec())
            .map_err(|source| (CLIENT_LAYER, source.to_string()))?;
        let expires = began
            .checked_add(KEYSTROKE_DEADLINE)
            .ok_or((CLIENT_LAYER, "no clock".to_owned()))?;
        let mut came = false;
        while !came && Instant::now() < expires {
            let left = expires.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(ManagerEvent::Bytes {
                    pane: named, bytes, ..
                }) if named == pane => {
                    came = true;
                    credit(manager, alias, pane, bytes.len());
                }
                Ok(_otherwise) => {}
                Err(_nothing) => break,
            }
        }
        if !came {
            let _ended = close(manager, alias, session);
            return Err((
                TRANSPORT_LAYER,
                format!("a keystroke never came back from {alias}"),
            ));
        }
        taken.push(began.elapsed());
    }
    // The session this made is this command's, and goes with it: a benchmark
    // that left a shell running on every host it measured would be a leak
    // somebody found months later.
    let _ended = close(manager, alias, session);
    Ok(taken)
}

/// Takes everything the pane has already said, and pays for it.
///
/// Credit is the reader's to return, and one that took bytes without
/// returning any would draw the host's window down until it stopped sending —
/// which from here looks exactly like a host that stopped answering.
fn settled(
    manager: &HostManager,
    events: &std::sync::mpsc::Receiver<ManagerEvent>,
    alias: &str,
    pane: PaneId,
) {
    while let Ok(event) = events.try_recv() {
        if let ManagerEvent::Bytes {
            pane: named, bytes, ..
        } = event
            && named == pane
        {
            credit(manager, alias, pane, bytes.len());
        }
    }
}

/// Returns credit for what was consumed.
fn credit(manager: &HostManager, alias: &str, pane: PaneId, taken: usize) {
    let owed = u32::try_from(taken).unwrap_or(u32::MAX);
    let _returned = manager.credit(alias, pane, owed);
}

/// Closes the session this command made, and says whether it was asked for.
fn close(manager: &HostManager, alias: &str, session: SessionId) -> bool {
    manager
        .command(alias, SessionCommand::CloseSession { session })
        .is_ok()
}

/// Makes a session on the host and gives back it and the pane it comes with.
///
/// A pane of its own rather than one somebody is using: what is typed here
/// appears on the screen it is typed into, and a benchmark has no business
/// writing on somebody's work.
///
/// # Errors
///
/// The layer and what it said.
fn a_pane_of_its_own(
    manager: &HostManager,
    events: &std::sync::mpsc::Receiver<ManagerEvent>,
    alias: &str,
) -> Result<(SessionId, PaneId), (&'static str, String)> {
    let submitted = manager
        .command(
            alias,
            SessionCommand::CreateSession {
                name: "benchmark".to_owned(),
                columns: COLUMNS,
                rows: ROWS,
                working_directory: None,
            },
        )
        .map_err(|source| (CLIENT_LAYER, source.to_string()))?;
    let expires = Instant::now()
        .checked_add(PANE_DEADLINE)
        .ok_or((CLIENT_LAYER, "no clock".to_owned()))?;
    // The session this command made, learned from the answer to the command
    // that made it — never by looking for one with the right name, which
    // would find the one a run before this left behind.
    while Instant::now() < expires {
        let left = expires.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            break;
        };
        let ManagerEvent::Notify(Notification::CommandFinished {
            command, outcome, ..
        }) = event
        else {
            continue;
        };
        if command != submitted.id {
            continue;
        }
        let CommandOutcome::Applied {
            created: Created::Session(session),
            ..
        } = outcome
        else {
            return Err((
                TRANSPORT_LAYER,
                format!("{alias} would not make a session: {outcome:?}"),
            ));
        };
        // The answer comes before the change is announced — the host says
        // what it did and then says what it is — so the session named in it
        // is not in the model this holds until the delta arrives.
        while Instant::now() < expires {
            if let Some(found) = pane_of(manager, alias, session) {
                return Ok((session, found));
            }
            let waiting = expires.saturating_duration_since(Instant::now());
            if events.recv_timeout(waiting).is_err() {
                break;
            }
        }
        return Err((
            TRANSPORT_LAYER,
            format!("{alias} never said what was in session {}", session.0),
        ));
    }
    Err((
        TRANSPORT_LAYER,
        format!(
            "{alias} never answered the command {} that asked for a session",
            submitted.id.0
        ),
    ))
}

/// The pane of one session, if the host has said it has one.
fn pane_of(manager: &HostManager, alias: &str, session: SessionId) -> Option<PaneId> {
    let held = manager.model();
    let view = held.host(&HostId(alias.to_owned()))?;
    view.settled
        .sessions
        .iter()
        .find(|holding| holding.id == session)?
        .tabs
        .first()?
        .panes
        .first()
        .map(|pane| pane.id)
}

/// The distribution as one object.
pub(crate) fn shaped(alias: &str, taken: &[Duration]) -> Value {
    let mut sorted: Vec<Duration> = taken.to_vec();
    sorted.sort_unstable();
    object(vec![
        ("host", text(alias)),
        (
            "keystrokes",
            Value::Whole(u64::try_from(sorted.len()).unwrap_or(0)),
        ),
        ("unit", text("milliseconds")),
        ("minimum", at(&sorted, QUICKEST)),
        ("median", at(&sorted, MEDIAN)),
        ("ninety_ninth", at(&sorted, NINETY_NINTH)),
        ("maximum", at(&sorted, SLOWEST)),
    ])
}

/// The time at one place in the sorted times, in milliseconds.
///
/// The place is a fraction of the way along rather than a proportion in a
/// float: a hundred times have a hundred places, and where the ninety-ninth
/// of them is, is arithmetic on whole numbers.
fn at(sorted: &[Duration], place: (usize, usize)) -> Value {
    let last = sorted.len().saturating_sub(1);
    let (numerator, denominator) = place;
    let index = last
        .saturating_mul(numerator)
        .checked_div(denominator)
        .unwrap_or(0);
    sorted.get(index.min(last)).map_or(Value::Null, |held| {
        Value::Fraction(held.as_secs_f64() * MILLISECONDS)
    })
}
