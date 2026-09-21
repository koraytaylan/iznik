//! The engine behind the window, driven the way the window drives it.
//!
//! Two kinds of case here, and they are deliberately different. The first three
//! drive [`EngineState`] with values — a thousand generated model sequences,
//! one answer, one connection that offers an upgrade — because what they ask
//! about is what the window does with an event, and a daemon would only put
//! that answer further away. Each says exactly that in its `because` in
//! `regression/claims/engine-bridge.toml`.
//!
//! The last drives the whole thing against the real in-process stack, from
//! inside GPUI's test context: a `unix:` alias to a daemon on this machine, the
//! bridge's engine behind an entity GPUI owns, and the state a window would
//! read. It is the same daemon the client's own `connection_manager` cases
//! stand up, reached one layer higher, and it is what says the bridge's own
//! threads do their jobs.

use core::time::Duration;
use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::AppContext as _;
use gpui_kit::Entity;
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::host_ui::{EngineState, HostReport, HostUi, Notice, NoticeKind};
use iznik_client::bootstrap::probe::InstalledServer;
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{ManagerEvent, ManagerOptions};
use iznik_client::host::state::{BackoffPolicy, HostState, UpgradeOffer, UpgradeReason};
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX};
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{
    CommandOutcome, RejectionCode, SessionCommand, encode_command_outcome,
};
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{CommandId, Generation, SessionId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, encode_host_model};
use iznik_testkit::generate::ModelGenerator;
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long a case waits between looks while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The backoff these cases run under: tens of milliseconds, so a case about a
/// host that fails takes no longer than one about anything else.
const QUICK_INITIAL: Duration = Duration::from_millis(20);

/// And a ceiling under a second.
const QUICK_MAXIMUM: Duration = Duration::from_millis(200);

/// The width the sessions the live case makes are made at.
const COLUMNS: u16 = 80;

/// And their height.
const ROWS: u16 = 24;

/// How many generated sequences the convergence case walks.
const SEQUENCES: usize = 1_000;

/// How many changes each of them carries.
const CHANGES: usize = 5;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_2117_0007;

/// The number this client gives the command the refusal case answers.
const ANSWERED: u64 = 7;

/// The version a scripted host is running.
const INSTALLED: &str = "1.2.3";

/// And the newer one this build carries and offers instead.
const BUNDLED: &str = "1.2.5";

/// The protocol both of them speak.
const PROTOCOL: u16 = 1;

/// The two session names the live case makes, in the order it makes them.
const FIRST_SESSION: &str = "work";

/// And the second, which is only there so the refusal has something to leave
/// alone.
const SECOND_SESSION: &str = "build";

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
    let path = base.join(format!("iznik-app-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// The engine these cases run, with an empty artifacts directory: nothing
/// reached through `unix:` is ever installed.
///
/// # Errors
///
/// When the runtime paths cannot be made or the engine cannot be started.
fn engine(held: &Scratch) -> Result<EngineBridge, Failed> {
    let artifacts = held.path.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    let mut options = ManagerOptions::new(artifacts, paths);
    options.backoff = BackoffPolicy {
        initial: QUICK_INITIAL,
        maximum: QUICK_MAXIMUM,
        ..BackoffPolicy::default()
    };
    options.channel = ChannelOptions {
        ping_interval: Duration::from_millis(50),
        pong_deadline: Duration::from_millis(400),
        open_deadline: Duration::from_secs(5),
        // A server on this machine greets in microseconds; one that has not in
        // three hundred milliseconds is not one of these cases' hosts.
        greeting_deadline: Duration::from_millis(300),
    };
    options.expire_interval = Duration::from_millis(50);
    options.pending_command_timeout = Duration::from_millis(300);
    Ok(EngineBridge::under(options)?)
}

/// A runtime for the daemon the live case stands up.
///
/// The bridge's engine owns its own runtime, and none of its methods may be
/// called from inside this one. This exists to start a stack and to let its
/// task run; nothing else is driven on it.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// The alias that reaches a socket on this machine.
fn alias(socket: &std::path::Path) -> String {
    format!("{LOCAL_PREFIX}{}", socket.display())
}

/// The model the window's mirror holds for `host`.
///
/// # Errors
///
/// When the window knows no such host.
fn mirrored(state: &EngineState, host: &HostId) -> Result<HostModel, Failed> {
    state
        .model()
        .host(host)
        .map(|view| view.model.clone())
        .ok_or_else(|| format!("{host} is not known to the window").into())
}

/// The snapshot message that carries `model`.
///
/// # Errors
///
/// When the model cannot be encoded.
fn snapshot(model: &HostModel) -> Result<ToClient, Failed> {
    Ok(ToClient::Snapshot {
        generation: model.generation,
        payload: encode_host_model(model)?,
    })
}

/// The delta message that carries `delta` as the model's next change.
///
/// # Errors
///
/// When the change cannot be encoded.
fn numbered(at: Generation, delta: &Delta) -> Result<ToClient, Failed> {
    Ok(ToClient::Delta {
        generation: Generation(at.0.saturating_add(1)),
        payload: encode_delta(delta)?,
    })
}

/// Whether the window's mirror holds a session by that name on `host`.
fn holds_session(ui: &HostUi, host: &HostId, name: &str) -> bool {
    ui.state()
        .model()
        .host(host)
        .is_some_and(|view| view.model.sessions.iter().any(|held| held.name == name))
}

/// A server of one version, for an offer to name.
fn installed(version: &str) -> InstalledServer {
    InstalledServer {
        crate_version: version.to_owned(),
        protocol_version: PROTOCOL,
    }
}

/// # Panics
///
/// When a window fed a host's snapshots and deltas does not arrive where that
/// host is.
#[test]
fn engine_bridge_mirror_converges_on_the_model_the_host_holds() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let host = HostId(FIRST_SESSION.to_owned());
        for index in 0..SEQUENCES {
            let start = generator.model();
            let sequence = generator.changes(&start, CHANGES);
            let mut state = EngineState::new();
            // The first snapshot is what makes the window know the host at all,
            // which is what a connection's model is for.
            state.apply(&host, &snapshot(&sequence.start)?);
            let halfway = sequence.changes.len().checked_div(2).unwrap_or(0);
            for (at, change) in sequence.changes.iter().enumerate() {
                for delta in change {
                    let generation = mirrored(&state, &host)?.generation;
                    state.apply(&host, &numbered(generation, delta)?);
                }
                // Halfway through, the host sends its whole model instead,
                // which is what a reconnection does, and the rest of the deltas
                // must carry on from it.
                if at == halfway {
                    let standing = mirrored(&state, &host)?;
                    state.apply(&host, &snapshot(&standing)?);
                }
            }
            assert_eq!(
                mirrored(&state, &host)?,
                sequence.finish,
                "sequence {index}: the window's mirror did not arrive where the host is"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a refused command moves the mirror, or when the refusal is not said
/// aloud naming what was refused.
#[test]
fn engine_bridge_leaves_the_mirror_alone_when_a_command_is_refused() {
    let case = || -> Result<(), Failed> {
        let host = HostId(FIRST_SESSION.to_owned());
        let mut generator = ModelGenerator::new(SEED);
        let start = generator.model();
        let mut state = EngineState::new();
        state.apply(&host, &snapshot(&start)?);
        let before = mirrored(&state, &host)?;

        // A command the host refuses: it names a session no host holds, so
        // nothing about the model may move. The answer goes in through the same
        // reducer the engine uses, which is what makes "nothing moved" a fact
        // about this code rather than about a copy of it.
        let outcome = CommandOutcome::Rejected {
            code: RejectionCode::UnknownSession,
            message: "no session of that number".to_owned(),
        };
        state.apply(
            &host,
            &ToClient::CommandResult {
                command_id: CommandId(ANSWERED),
                payload: encode_command_outcome(&outcome)?,
            },
        );
        assert_eq!(
            mirrored(&state, &host)?,
            before,
            "a refusal leaves the mirror exactly as the host last said it was"
        );

        let notices = state.take_notices();
        let refusal = notices
            .iter()
            .find(|notice| notice.kind == NoticeKind::Refusal)
            .ok_or_else(|| format!("no refusal was said aloud: {notices:?}"))?;
        assert_eq!(
            refusal.host, host,
            "the refusal names the host it was about"
        );
        assert!(
            refusal.detail.contains("UnknownSession"),
            "and names the code the host refused it with: {}",
            refusal.detail
        );
        assert!(
            refusal.detail.contains(&ANSWERED.to_string()),
            "and which command it was, so whoever sent it knows: {}",
            refusal.detail
        );
        assert!(
            !notices
                .iter()
                .any(|notice| notice.kind == NoticeKind::Failure),
            "a refusal is its own kind and not a failure: {notices:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a host's upgrade offer is not held as state a dialog can render, or
/// when it is reported as an error.
#[test]
fn engine_bridge_holds_the_upgrade_offer_as_state() {
    let case = || -> Result<(), Failed> {
        let host = HostId(SECOND_SESSION.to_owned());
        let mut state = EngineState::new();
        state.absorb(EngineEvent::Said(ManagerEvent::Moved {
            host: host.clone(),
            state: HostState::Connected {
                server_version: INSTALLED.to_owned(),
                capabilities: Capabilities::REORDER_SESSIONS,
                upgrade: Some(UpgradeOffer {
                    installed: installed(INSTALLED),
                    bundled: installed(BUNDLED),
                    reason: UpgradeReason::Version,
                }),
            },
        }));

        // State a dialog renders, and the whole of what it needs: what the host
        // runs, what this build carries, and the state it arrived with.
        let offer = state
            .upgrade_offer(&host)
            .ok_or("the offer the connection carried is not held")?;
        assert_eq!(offer.installed.crate_version, INSTALLED);
        assert_eq!(offer.bundled.crate_version, BUNDLED);
        assert!(
            matches!(
                state.host(&host).map(|report| &report.connection),
                Some(HostState::Connected { .. })
            ),
            "and the connection that brought it is held beside it"
        );

        let notices = state.take_notices();
        assert!(
            notices
                .iter()
                .any(|notice| notice.kind == NoticeKind::Offer && notice.detail.contains(BUNDLED)),
            "the offer is announced as an offer: {notices:?}"
        );
        assert!(
            !notices
                .iter()
                .any(|notice| notice.kind == NoticeKind::Failure
                    || notice.kind == NoticeKind::Refusal),
            "an offer is neither a failure nor a refusal: {notices:?}"
        );

        // A connection that carries no offer clears it: a host upgraded since
        // is not one still asking, and an offer nothing could dismiss would be
        // a question with no answer.
        state.absorb(EngineEvent::Said(ManagerEvent::Moved {
            host: host.clone(),
            state: HostState::Connected {
                server_version: BUNDLED.to_owned(),
                capabilities: Capabilities::REORDER_SESSIONS,
                upgrade: None,
            },
        }));
        assert_eq!(
            state.upgrade_offer(&host),
            None,
            "a connection with nothing on offer stops the asking"
        );
        assert!(
            state.host(&host).is_some(),
            "and the host is still one the window knows"
        );

        // A host nobody has said anything about has no report, and the report a
        // host starts from says so.
        assert_eq!(
            state.host(&HostId("nowhere".to_owned())),
            None,
            "a host nobody named has no report"
        );
        assert_eq!(HostReport::unknown().connection, HostState::Disconnected);
        assert_eq!(HostReport::unknown().upgrade, None);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the engine's own threads do not carry a local socket host from probing to
/// connected, a command into the mirror, a refusal into a notice, and the
/// host's removal back out again — all inside GPUI's test context, which is
/// where this crate's cases run.
#[gpui_kit::test]
fn engine_bridge_carries_a_local_socket_host_into_the_window(
    context: &mut gpui_kit::TestAppContext,
) {
    context.update(gpui_kit::init);
    assert_the_live_case(context);
}

/// Runs the live case and reports what it failed on.
///
/// # Panics
///
/// When the live case fails, saying what the stage it stopped in was. An
/// assertion and not a `panic!`, for the same reason the headless smoke's own
/// wrapper is: the enclosing annotation moves its documentation onto the
/// generated wrapper, so the failing assertion lives one call below it.
fn assert_the_live_case(context: &gpui_kit::TestAppContext) {
    let outcome = carry_a_local_socket_host(context);
    assert!(outcome.is_ok(), "the live case: {outcome:?}");
}

/// The live case: a daemon on this machine behind a `unix:` alias, the bridge's
/// engine behind an entity GPUI owns, and the state a window would read.
///
/// Staged rather than written end to end, because what it establishes is four
/// separate obligations — the states a host moves through, a command and the
/// change it caused, a refusal that leaves the mirror alone, and a host let go
/// through the thread that waits — and each is read on its own below. Every
/// stage reports through `Result`, so the only panic here is an assertion
/// about what the window turned out to be told.
///
/// # Errors
///
/// When any stage fails, saying what failed.
///
/// # Panics
///
/// When a command the host refused was given a number no later than one that
/// worked, or a host let go was not said to be no longer held.
fn carry_a_local_socket_host(context: &gpui_kit::TestAppContext) -> Result<(), Failed> {
    let held = scratch("unix")?;
    let runtime = runtime()?;
    let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
    let host = HostId(alias(stack.socket()));
    let engine = engine(&held)?;

    // The entity GPUI owns: the whole of the application's engine state
    // lives behind this handle, and the window reads it through it.
    let view: Entity<HostUi> = context.update(|app| app.new(|_context| HostUi::new(engine)));
    arrive_at_connected(context, &host, &view)?;
    let asked = make_sessions(context, &host, &view)?;
    let refused = refuse_a_rename(context, &host, &view)?;
    assert!(
        refused.0 > asked.0,
        "the refused command was given a later number than the one that worked"
    );
    let gone = let_the_host_go(context, &host, &view)?;
    assert!(
        gone.iter()
            .any(|notice| notice.detail.contains("no longer held")),
        "the window is told the host is no longer held: {gone:?}"
    );

    drop(stack);
    Ok(())
}

/// Adds the host and watches it arrive at connected, asserting the states the
/// window was told on the way.
///
/// # Errors
///
/// When the host does not connect inside the case's prompt, or the manager
/// will not take the host.
///
/// # Panics
///
/// When the states reported are not a probing that gave way to a connected.
fn arrive_at_connected(
    context: &gpui_kit::TestAppContext,
    host: &HostId,
    view: &Entity<HostUi>,
) -> Result<(), Failed> {
    context
        .update(|app| view.update(app, |ui, _context| ui.add_host(&host.0)))
        .map_err(|refusal| format!("the host is added: {refusal}"))?;

    // The states a host goes through are reported in the order it goes
    // through them, and connected is where it arrives.
    let told = context.update(|app| {
        view.update(app, |ui, _context| {
            absorb_until(ui, "a connected host", |ui, _said| {
                matches!(
                    ui.state().host(host).map(|report| &report.connection),
                    Some(HostState::Connected { .. })
                )
            })
        })
    })?;
    assert!(
        context.update(|app| { view.read_with(app, |ui, _app| ui.state().host(host).is_some()) }),
        "the host is one the window knows; told {told:?}"
    );
    let states: Vec<String> = told
        .iter()
        .filter(|notice| notice.kind == NoticeKind::Connection)
        .map(|notice| notice.detail.clone())
        .collect();
    assert!(
        states.iter().any(|state| state.starts_with("probing")),
        "the window is told the host is probing: {states:?}"
    );
    assert!(
        states.iter().any(|state| state.starts_with("connected")),
        "and told when it is connected: {states:?}"
    );
    Ok(())
}

/// Creates two sessions by command, holding each in the mirror as its answer
/// and the change it caused arrive.
///
/// The second is only there so that the refusal below has something to leave
/// alone.
///
/// # Errors
///
/// When either session does not show in the mirror inside the case's prompt,
/// or the engine refuses either command.
///
/// # Panics
///
/// When the window is not told that the first of them was applied.
fn make_sessions(
    context: &gpui_kit::TestAppContext,
    host: &HostId,
    view: &Entity<HostUi>,
) -> Result<CommandId, Failed> {
    // A session made by command: the answer arrives, and then the change it
    // caused shows in the mirror the window draws from.
    let asked = context.update(|app| {
        view.update(app, |ui, _context| {
            ui.command(&host.0, create(FIRST_SESSION))
                .map(|submission| submission.id)
        })
    });
    let asked = asked.map_err(|refusal| format!("the session command: {refusal}"))?;
    assert!(asked.0 > 0, "the command was given a number");

    let made = context.update(|app| {
        view.update(app, |ui, _context| {
            absorb_until(ui, "the session in the mirror", |ui, _said| {
                holds_session(ui, host, FIRST_SESSION)
            })
        })
    })?;
    assert!(
        made.iter().any(|notice| notice.kind == NoticeKind::Command),
        "the window is told the command was applied: {made:?}"
    );

    context
        .update(|app| {
            view.update(app, |ui, _context| {
                ui.command(&host.0, create(SECOND_SESSION))
            })
        })
        .map_err(|refusal| format!("a second session was asked for: {refusal}"))?;
    context.update(|app| {
        view.update(app, |ui, _context| {
            absorb_until(ui, "both sessions", |ui, notices| {
                holds_session(ui, host, FIRST_SESSION)
                    && holds_session(ui, host, SECOND_SESSION)
                    && notices
                        .iter()
                        .any(|notice| notice.kind == NoticeKind::Command)
            })
        })
    })?;
    Ok(asked)
}

/// Renames a session no host holds — a command the host refuses — and asserts
/// the mirror is left exactly as the host last said it was.
///
/// # Errors
///
/// When the refusal does not surface inside the case's prompt, or the engine
/// will not take the command.
///
/// # Panics
///
/// When the mirror no longer holds both sessions the host was told to make.
fn refuse_a_rename(
    context: &gpui_kit::TestAppContext,
    host: &HostId,
    view: &Entity<HostUi>,
) -> Result<CommandId, Failed> {
    let refused = context.update(|app| {
        view.update(app, |ui, _context| {
            ui.command(
                &host.0,
                SessionCommand::RenameSession {
                    session: SessionId(u64::MAX),
                    name: "nowhere".to_owned(),
                },
            )
            .map(|submission| submission.id)
        })
    });
    let refused = refused.map_err(|refusal| format!("the refused rename: {refusal}"))?;

    let told = context.update(|app| {
        view.update(app, |ui, _context| {
            absorb_until(ui, "the refusal", |ui, notices| {
                ui.state().model().host(host).is_some_and(|mirrored| {
                    mirrored.model.sessions.len() == 2
                        && mirrored.settled.generation > Generation(0)
                }) && notices
                    .iter()
                    .any(|notice| notice.kind == NoticeKind::Refusal)
            })
        })
    })?;
    assert!(
        told.iter().any(|notice| {
            notice.kind == NoticeKind::Refusal && notice.detail.contains("UnknownSession")
        }),
        "the refusal surfaced as a notice naming it: {told:?}"
    );
    context.update(|app| {
        view.read_with(app, |ui, _app| {
            assert!(
                holds_session(ui, host, FIRST_SESSION) && holds_session(ui, host, SECOND_SESSION),
                "and the mirror still holds exactly what the host said"
            );
        });
    });
    Ok(refused)
}

/// Lets the host go through the operations thread, which is the path that
/// waits on a host's task and so may not be the drawing thread's.
///
/// Nothing here waits on the order: it is placed, and what it did arrives as a
/// notice like everything else. The notices are returned rather than asserted
/// on, so that the live case keeps every assertion it makes in one place.
///
/// # Errors
///
/// When the engine will not take the order, or the host is not gone inside the
/// case's prompt.
fn let_the_host_go(
    context: &gpui_kit::TestAppContext,
    host: &HostId,
    view: &Entity<HostUi>,
) -> Result<Vec<Notice>, Failed> {
    context
        .update(|app| view.update(app, |ui, _context| ui.remove_host(&host.0)))
        .map_err(|refusal| format!("the removal is ordered: {refusal}"))?;
    context.update(|app| {
        view.update(app, |ui, _context| {
            absorb_until(ui, "the host let go", |ui, _said| {
                ui.state().host(host).is_none() && ui.state().model().host(host).is_none()
            })
        })
    })
}

/// A new session command for a session called `name`.
fn create(name: &str) -> SessionCommand {
    SessionCommand::CreateSession {
        name: name.to_owned(),
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
    }
}

/// Takes everything the engine says until `wanted` holds, over what has been
/// told so far as well as what the window now holds.
///
/// Returns every notice it passed on the way, so that what the window was told
/// is what a case asserts against rather than a second reading of it. The
/// predicate sees the accumulation and not only the last batch, because an
/// answer and the change it caused arrive as two events and a case that looked
/// at one batch would call the first of them the whole of it.
///
/// # Errors
///
/// When `wanted` never holds, saying what the window held and what it was told.
fn absorb_until<Wanted>(ui: &mut HostUi, what: &str, wanted: Wanted) -> Result<Vec<Notice>, Failed>
where
    Wanted: Fn(&HostUi, &[Notice]) -> bool,
{
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    let mut said = Vec::new();
    while Instant::now() < expires {
        ui.absorb();
        said.extend(ui.take_notices());
        if wanted(ui, &said) {
            return Ok(said);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Err(format!(
        "no {what} inside {PROMPT:?}; the window knows {:?} and was told {said:?}",
        ui.state()
            .hosts()
            .map(|(host, _report)| host.clone())
            .collect::<Vec<_>>()
    )
    .into())
}
