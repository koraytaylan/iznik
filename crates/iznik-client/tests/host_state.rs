//! A host's name, a pane's address, and the table that is a host's life.
//!
//! Nothing here waits for anything. The state machine takes the moment as an
//! argument and draws its jitter from a seed, so a backoff that grows over
//! minutes is checked in microseconds — which is the only way a case about
//! what happens when a laptop closes gets run often enough to be worth having.

use core::time::Duration;
use std::time::Instant;

use iznik_client::bootstrap::launch::Stage;
use iznik_client::bootstrap::probe::InstalledServer;
use iznik_client::host::identity::{ADDRESS_SCHEME, AddressError, GlobalPaneId, HostId};
use iznik_client::host::state::{
    Action, BACKOFF_INITIAL, BACKOFF_MAXIMUM, BackoffPolicy, HostEvent, HostState,
    HostStateMachine, UpgradeOffer,
};
use iznik_protocol::identity::PaneId;

/// The pane these cases address.
const PANE: PaneId = PaneId(7);

/// A short first wait, so a whole backoff is checked in microseconds.
const QUICK_INITIAL: Duration = Duration::from_millis(10);

/// A ceiling it reaches after three doublings.
const QUICK_MAXIMUM: Duration = Duration::from_millis(80);

/// How many failures the backoff case walks through.
const FAILURES: u32 = 6;

/// A seed of this case's own.
const SEED: u64 = 0x0123_4567_89ab_cdef;

/// Another, for the case about two hosts failing together.
const OTHER_SEED: u64 = 0xfedc_ba98_7654_3210;

/// What a scripted server says it is.
const SERVER_VERSION: &str = "0.1.0";

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// One action, without the moment inside it: what a table can compare.
fn shape(action: &Action) -> String {
    match action {
        Action::Bootstrap => "bootstrap".to_owned(),
        Action::StopBootstrap => "stop-bootstrap".to_owned(),
        Action::CloseChannel => "close-channel".to_owned(),
        Action::Resume => "resume".to_owned(),
        Action::RetryAt(_moment) => "retry-at".to_owned(),
        Action::Forget => "forget".to_owned(),
    }
}

/// The shapes of a list of them.
fn shapes(actions: &[Action]) -> Vec<String> {
    actions.iter().map(shape).collect()
}

/// The upgrade a connected host may be carrying.
fn offer() -> UpgradeOffer {
    UpgradeOffer {
        installed: InstalledServer {
            crate_version: "0.0.1".to_owned(),
            protocol_version: 1,
        },
        bundled: InstalledServer {
            crate_version: SERVER_VERSION.to_owned(),
            protocol_version: 1,
        },
    }
}

/// A host that has connected.
fn connected() -> HostEvent {
    HostEvent::Connected {
        server_version: SERVER_VERSION.to_owned(),
        upgrade: None,
    }
}

/// A machine driven to `named`, by the events that get it there.
fn machine_in(named: &str, now: Instant) -> HostStateMachine {
    let mut machine = HostStateMachine::new(BackoffPolicy::default());
    let prelude: Vec<HostEvent> = match named {
        "probing" => vec![HostEvent::Added],
        "bootstrapping" => vec![
            HostEvent::Added,
            HostEvent::Reached {
                stage: Stage::Upload,
            },
        ],
        "connecting" => vec![
            HostEvent::Added,
            HostEvent::Reached {
                stage: Stage::Launch,
            },
        ],
        "connected" => vec![HostEvent::Added, connected()],
        "reconnecting" => vec![
            HostEvent::Added,
            connected(),
            HostEvent::LinkDead {
                detail: "silent".to_owned(),
            },
        ],
        "failed" => vec![
            HostEvent::Added,
            HostEvent::Failed {
                error: "no route".to_owned(),
            },
        ],
        _disconnected => Vec::new(),
    };
    for event in prelude {
        let _actions = machine.on(event, now);
    }
    machine
}

/// # Panics
///
/// When an address does not render as the architecture says, or does not parse
/// back to what it was.
#[test]
fn host_identity_renders_and_parses_one_address() {
    let held = GlobalPaneId {
        host: HostId("work".to_owned()),
        pane: PANE,
    };
    assert_eq!(
        held.to_string(),
        format!("{ADDRESS_SCHEME}work/{}", PANE.0),
        "the canonical rendering"
    );
    assert_eq!(
        GlobalPaneId::parse(&held.to_string()),
        Ok(held),
        "and it reads back to itself"
    );
}

/// # Panics
///
/// When an alias with characters that need escaping does not round-trip.
#[test]
fn host_identity_carries_an_alias_that_needs_escaping() {
    // A `unix:` alias holds a colon and slashes; an SSH alias may hold
    // anything a person typed. Every one of them must come back exactly, or
    // the address is not an address.
    for alias in [
        "unix:/tmp/iznik.sock",
        "work",
        "build box",
        "user@host:2222",
        "a/b",
        "100%",
        "h\u{f4}te",
    ] {
        let held = GlobalPaneId {
            host: HostId(alias.to_owned()),
            pane: PANE,
        };
        let written = held.to_string();
        assert!(
            written.starts_with(ADDRESS_SCHEME),
            "it is still an iznik address: {written}"
        );
        assert_eq!(
            GlobalPaneId::parse(&written),
            Ok(held),
            "and {alias:?} comes back exactly from {written}"
        );
    }
}

/// # Panics
///
/// When something that is not an address is read as one.
#[test]
fn host_identity_refuses_what_is_not_an_address() {
    let refused = [
        ("work/7", "no scheme"),
        ("iznik://work", "no pane"),
        ("iznik:///7", "no host"),
        ("iznik://work/seven", "a pane that is not a number"),
        ("iznik://wo%zz/7", "an escape that is not hexadecimal"),
        ("iznik://wo%f/7", "an escape cut short"),
    ];
    for (given, why) in refused {
        assert!(
            GlobalPaneId::parse(given).is_err(),
            "{given:?} is refused, being {why}"
        );
    }
    assert_eq!(
        GlobalPaneId::parse("work/7"),
        Err(AddressError::Scheme {
            given: "work/7".to_owned(),
        }),
        "and the refusal says which part it was"
    );
}

/// # Panics
///
/// When a `unix:` alias is not recognized as a socket on this machine, or an
/// SSH alias is.
#[test]
fn host_identity_knows_a_socket_from_a_host() {
    let local = HostId("unix:/tmp/iznik.sock".to_owned());
    assert_eq!(
        local.local_socket(),
        Some(std::path::PathBuf::from("/tmp/iznik.sock")),
        "the socket a `unix:` alias names"
    );
    assert!(!local.is_remote(), "and it is not reached over SSH");
    let remote = HostId("work".to_owned());
    assert_eq!(remote.local_socket(), None, "an alias names no socket");
    assert!(remote.is_remote(), "and is reached over SSH");
}

/// # Panics
///
/// When any pair of state and event does not produce the state and the actions
/// the architecture says it does.
#[test]
fn host_state_is_the_table_it_says_it_is() {
    let now = Instant::now();
    let table: &[(&str, HostEvent, &str, &[&str])] = &[
        ("disconnected", HostEvent::Added, "probing", &["bootstrap"]),
        ("disconnected", HostEvent::RetryDue, "disconnected", &[]),
        // Every stage of a bootstrap moves it, and the two that are about
        // reaching the server are one state from outside.
        (
            "probing",
            HostEvent::Reached {
                stage: Stage::Upload,
            },
            "bootstrapping, uploading",
            &[],
        ),
        (
            "bootstrapping",
            HostEvent::Reached {
                stage: Stage::Launch,
            },
            "connecting",
            &[],
        ),
        (
            "bootstrapping",
            HostEvent::Reached {
                stage: Stage::Handshake,
            },
            "connecting",
            &[],
        ),
        // Failure at any stage holds it with a time to try again.
        (
            "probing",
            HostEvent::Failed {
                error: "no route".to_owned(),
            },
            "failed: no route",
            &["retry-at"],
        ),
        (
            "bootstrapping",
            HostEvent::Failed {
                error: "disk full".to_owned(),
            },
            "failed: disk full",
            &["retry-at"],
        ),
        (
            "connecting",
            HostEvent::Failed {
                error: "another version".to_owned(),
            },
            "failed: another version",
            &["retry-at"],
        ),
        (
            "connecting",
            connected(),
            "connected to 0.1.0",
            // Every subscription is resumed at the byte the model holds.
            &["resume"],
        ),
        // A link that dies under a connected host is the case the whole
        // backoff exists for.
        (
            "connected",
            HostEvent::LinkDead {
                detail: "silent for 10s".to_owned(),
            },
            "reconnecting, attempt 1",
            &["close-channel", "retry-at"],
        ),
        (
            "reconnecting",
            HostEvent::RetryDue,
            "probing",
            &["bootstrap"],
        ),
        // Somebody asking for a waiting host does not wait out its backoff.
        ("reconnecting", HostEvent::Added, "probing", &["bootstrap"]),
        ("failed", HostEvent::Added, "probing", &["bootstrap"]),
        ("failed", HostEvent::RetryDue, "probing", &["bootstrap"]),
        // A retry that fires late must not start a second bootstrap.
        ("probing", HostEvent::RetryDue, "probing", &[]),
        ("probing", HostEvent::Added, "probing", &[]),
        ("connected", HostEvent::Added, "connected to 0.1.0", &[]),
    ];
    for (from, event, wanted, actions) in table {
        let mut machine = machine_in(from, now);
        let taken = machine.on(event.clone(), now);
        assert_eq!(
            machine.state().to_string(),
            *wanted,
            "{from} + {event:?} goes to {wanted}"
        );
        assert_eq!(
            shapes(&taken),
            *actions,
            "{from} + {event:?} does exactly that"
        );
    }
}

/// # Panics
///
/// When removal from a state does not tear down exactly what that state holds.
#[test]
fn host_state_tears_down_what_each_state_holds() {
    let now = Instant::now();
    let table: &[(&str, &[&str])] = &[
        ("disconnected", &["forget"]),
        // A bootstrap in flight is stopped before the host is forgotten.
        ("probing", &["stop-bootstrap", "forget"]),
        ("bootstrapping", &["stop-bootstrap", "forget"]),
        ("connecting", &["stop-bootstrap", "forget"]),
        // A connected host has a channel, and it is closed.
        ("connected", &["close-channel", "forget"]),
        // A waiting one holds only a timer.
        ("reconnecting", &["forget"]),
        ("failed", &["forget"]),
    ];
    for (from, actions) in table {
        let mut machine = machine_in(from, now);
        let taken = machine.on(HostEvent::Removed, now);
        assert_eq!(shapes(&taken), *actions, "removed from {from}");
        assert_eq!(
            machine.state().to_string(),
            "disconnected",
            "and it is disconnected afterwards"
        );
    }
}

/// The moment a connected machine says it will try again, once its link dies.
///
/// It does not reconnect: the failure count is what the backoff grows on, and
/// a helper that quietly connected would measure the first wait every time.
///
/// # Errors
///
/// When it did not schedule one.
fn first_failure(machine: &mut HostStateMachine, now: Instant) -> Result<Instant, Failed> {
    scheduled(machine.on(
        HostEvent::LinkDead {
            detail: "silent".to_owned(),
        },
        now,
    ))
}

/// The moment it says it will try again after one more failed attempt.
///
/// # Errors
///
/// When it did not schedule one.
fn fail_again(machine: &mut HostStateMachine, now: Instant) -> Result<Instant, Failed> {
    let _tried = machine.on(HostEvent::RetryDue, now);
    scheduled(machine.on(
        HostEvent::Failed {
            error: "no route".to_owned(),
        },
        now,
    ))
}

/// The moment a list of actions schedules a retry for.
///
/// # Errors
///
/// When none of them does.
fn scheduled(actions: Vec<Action>) -> Result<Instant, Failed> {
    actions
        .into_iter()
        .find_map(|action| match action {
            Action::RetryAt(moment) => Some(moment),
            _other => None,
        })
        .ok_or_else(|| "no retry was scheduled".into())
}

/// # Panics
///
/// When the wait does not grow toward the maximum, leaves its band, or passes
/// the maximum.
#[test]
fn host_state_waits_longer_each_time_and_never_past_the_maximum() {
    let case = || -> Result<(), Failed> {
        let now = Instant::now();
        let policy = BackoffPolicy {
            initial: QUICK_INITIAL,
            maximum: QUICK_MAXIMUM,
            seed: SEED,
        };
        let mut machine = HostStateMachine::new(policy);
        let _started = machine.on(HostEvent::Added, now);
        let _greeted = machine.on(connected(), now);
        let mut delays = Vec::new();
        // One run of failures with no connection between them, which is what
        // the backoff is for: a host that is not coming back yet.
        for attempt in 1..=FAILURES {
            let moment = if attempt == 1 {
                first_failure(&mut machine, now)?
            } else {
                fail_again(&mut machine, now)?
            };
            let waited = moment.saturating_duration_since(now);
            let doubled = QUICK_INITIAL.saturating_mul(2_u32.saturating_pow(attempt - 1));
            let base = doubled.min(QUICK_MAXIMUM);
            assert!(
                waited <= base && waited >= base / 2,
                "attempt {attempt} waits within half its base and its base: {waited:?} of {base:?}"
            );
            assert!(
                waited <= QUICK_MAXIMUM,
                "and never past the maximum: {waited:?}"
            );
            delays.push(waited);
        }
        assert!(
            delays.windows(2).any(|pair| pair[0] != pair[1]),
            "and the waits are jittered rather than a fixed ladder: {delays:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When two hosts that failed at the same moment come back at the same moment.
#[test]
fn host_state_keeps_two_hosts_that_failed_together_apart() {
    let case = || -> Result<(), Failed> {
        let now = Instant::now();
        let mut moments = Vec::new();
        for seed in [SEED, OTHER_SEED] {
            let mut machine = HostStateMachine::new(BackoffPolicy {
                initial: QUICK_INITIAL,
                maximum: QUICK_MAXIMUM,
                seed,
            });
            let _started = machine.on(HostEvent::Added, now);
            let _connected = machine.on(connected(), now);
            moments.push(first_failure(&mut machine, now)?);
        }
        // Four hosts failing together is one laptop closing, and four
        // bootstraps starting together is the storm the jitter exists to stop.
        assert_ne!(
            moments.first(),
            moments.get(1),
            "two seeds do not schedule the same moment: {moments:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When connecting does not put the attempt count back to where it started.
#[test]
fn host_state_forgets_the_failures_once_it_is_connected() {
    let now = Instant::now();
    let mut machine = HostStateMachine::new(BackoffPolicy::default());
    let _started = machine.on(HostEvent::Added, now);
    let _greeted = machine.on(connected(), now);
    for _failed in 0..FAILURES {
        let _dead = machine.on(
            HostEvent::LinkDead {
                detail: "silent".to_owned(),
            },
            now,
        );
        let _retried = machine.on(HostEvent::RetryDue, now);
    }
    let _again = machine.on(connected(), now);
    let _gone = machine.on(
        HostEvent::LinkDead {
            detail: "silent".to_owned(),
        },
        now,
    );
    assert_eq!(
        machine.state().to_string(),
        "reconnecting, attempt 1",
        "a host that came back starts counting again"
    );
}

/// # Panics
///
/// When a connected host does not carry what its server said, or the offer to
/// replace it.
#[test]
fn host_state_carries_the_upgrade_it_was_offered() {
    let now = Instant::now();
    let mut machine = HostStateMachine::new(BackoffPolicy::default());
    let _started = machine.on(HostEvent::Added, now);
    let _connected = machine.on(
        HostEvent::Connected {
            server_version: SERVER_VERSION.to_owned(),
            upgrade: Some(offer()),
        },
        now,
    );
    // The daemon *is* the sessions: an older one is connected to as it is, and
    // the offer rides along for somebody to accept.
    assert_eq!(
        machine.state(),
        &HostState::Connected {
            server_version: SERVER_VERSION.to_owned(),
            upgrade: Some(offer()),
        },
        "both versions are held, and nothing was replaced"
    );
}

/// # Panics
///
/// When the defaults are not the ones the architecture names.
#[test]
fn host_state_defaults_to_the_policy_the_architecture_names() {
    let policy = BackoffPolicy::default();
    assert_eq!(policy.initial, BACKOFF_INITIAL, "a second to start");
    assert_eq!(policy.maximum, BACKOFF_MAXIMUM, "a minute at most");
    assert_eq!(BACKOFF_INITIAL, Duration::from_secs(1), "which is a second");
    assert_eq!(BACKOFF_MAXIMUM, Duration::from_mins(1), "and a minute");
}
