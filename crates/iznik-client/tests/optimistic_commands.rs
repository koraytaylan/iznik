//! Commands that show before the host has agreed to them.
//!
//! What each case asks is whether the picture on the screen is one the host
//! would recognize. So the deltas the host would send are written out here, by
//! hand, and applied to a copy through the protocol's own reconciler: if the
//! optimistic model and that copy are the same model, the client showed the
//! truth early rather than a guess.
//!
//! Nothing here waits. `submit` and `expire` take the moment as an argument,
//! so a case about a five-second timeout is microseconds of arithmetic.

use core::time::Duration;
use std::time::Instant;

use iznik_client::commands::{
    Confirmed, PENDING_COMMAND_TIMEOUT, Submission, confirm, expire, submit,
};
use iznik_client::host::identity::HostId;
use iznik_client::model::{ClientModel, HostView};
use iznik_client::reduce::Notification;
use iznik_client::reduce::reduce;
use iznik_protocol::command::{CommandOutcome, Created, Placement, RejectionCode, SessionCommand};
use iznik_protocol::delta::{Delta, RemovalReason, encode_delta};
use iznik_protocol::identity::{CommandId, Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, LayoutNode, SplitDirection};
use iznik_protocol::reconcile::apply_change;
use iznik_testkit::generate::ModelGenerator;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_2213_0006;

/// How many generated models the equality case walks.
const MODELS: usize = 200;

/// A name no generated model carries.
const RENAMED: &str = "renamed-by-the-client";

/// And another.
const AGAIN: &str = "renamed-again";

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The host these cases talk to.
fn work() -> HostId {
    HostId("work".to_owned())
}

/// A moment nothing is measured from.
fn moment() -> Instant {
    Instant::now()
}

/// The first session of a model.
///
/// # Errors
///
/// When it has none.
fn a_session(model: &HostModel) -> Result<SessionId, Failed> {
    model
        .sessions
        .first()
        .map(|held| held.id)
        .ok_or_else(|| "the generated model has no session".into())
}

/// The first tab of its first session.
///
/// # Errors
///
/// When it has none.
fn a_tab(model: &HostModel) -> Result<TabId, Failed> {
    model
        .sessions
        .first()
        .and_then(|session| session.tabs.first())
        .map(|held| held.id)
        .ok_or_else(|| "the generated model has no tab".into())
}

/// The first pane of that tab.
///
/// # Errors
///
/// When it has none.
fn a_pane(model: &HostModel) -> Result<PaneId, Failed> {
    model
        .sessions
        .first()
        .and_then(|session| session.tabs.first())
        .and_then(|tab| tab.panes.first())
        .map(|held| held.id)
        .ok_or_else(|| "the generated model has no pane".into())
}

/// The model `deltas` lead to from `start`, as the host's own reconciler makes
/// it — the far end, built without the client having any part in it.
///
/// # Errors
///
/// When a delta this case wrote is one the reconciler refuses.
fn as_the_host_would(start: &HostModel, deltas: &[Delta]) -> Result<HostModel, Failed> {
    let mut held = start.clone();
    for delta in deltas {
        apply_change(&mut held, delta)?;
    }
    Ok(held)
}

/// Whether two models are the same but for the number they stand at.
fn same_but_for_the_generation(left: &HostModel, right: &HostModel) -> bool {
    let mut aligned = right.clone();
    aligned.generation = left.generation;
    left == &aligned
}

/// An answer that applied a command.
fn applied() -> CommandOutcome {
    CommandOutcome::Applied {
        generation: Generation(2),
        created: Created::Nothing,
    }
}

/// An answer that refused one.
fn rejected() -> CommandOutcome {
    CommandOutcome::Rejected {
        code: RejectionCode::UnknownSession,
        message: "no".to_owned(),
    }
}

/// # Panics
///
/// When what a command shows is not what the host would have sent.
#[test]
fn optimistic_commands_show_exactly_what_the_host_would_send() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        for index in 0..MODELS {
            let model = generator.model();
            let session = a_session(&model)?;
            let tab = a_tab(&model)?;
            let order: Vec<TabId> = model
                .sessions
                .first()
                .map(|held| held.tabs.iter().rev().map(|found| found.id).collect())
                .unwrap_or_default();
            // Every command whose local effect is beyond doubt, beside the
            // change the host would announce for it.
            let table: Vec<(SessionCommand, Vec<Delta>)> = vec![
                (
                    SessionCommand::RenameSession {
                        session,
                        name: RENAMED.to_owned(),
                    },
                    vec![Delta::SessionRenamed {
                        session,
                        name: RENAMED.to_owned(),
                    }],
                ),
                (
                    SessionCommand::RenameTab {
                        tab,
                        name: RENAMED.to_owned(),
                    },
                    vec![Delta::TabRenamed {
                        tab,
                        name: RENAMED.to_owned(),
                    }],
                ),
                (
                    SessionCommand::ReorderTabs {
                        session,
                        order: order.clone(),
                    },
                    vec![Delta::TabsReordered { session, order }],
                ),
                (
                    SessionCommand::CloseSession { session },
                    vec![Delta::SessionRemoved { session }],
                ),
            ];
            for (command, deltas) in table {
                let mut view = HostView::of(model.clone());
                let submission = submit(&mut view, command.clone(), moment());
                assert!(
                    submission.optimistic,
                    "model {index}: {command:?} shows at once"
                );
                assert!(
                    same_but_for_the_generation(&view.model, &as_the_host_would(&model, &deltas)?),
                    "model {index}: {command:?} showed something other than {deltas:?}"
                );
                // And the host agreeing changes nothing further.
                assert_eq!(
                    confirm(&mut view, submission.id, &applied()),
                    Confirmed::Applied,
                    "the answer retires it"
                );
                assert!(
                    view.pending.is_empty(),
                    "and nothing is left waiting for an answer"
                );
            }
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When closing a pane does not show the cascade the host would announce.
#[test]
fn optimistic_commands_show_the_cascade_a_pane_causes() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let mut cascaded = 0_usize;
        for index in 0..MODELS {
            let model = generator.model();
            let pane = a_pane(&model)?;
            let tab = a_tab(&model)?;
            let session = a_session(&model)?;
            let Some(held) = model
                .sessions
                .first()
                .and_then(|found| found.tabs.first())
                .cloned()
            else {
                return Err("the generated model has no tab".into());
            };
            // A pane going takes its place in the layout with it — or, when it
            // was the only one, the tab, and the session behind that.
            let mut deltas = vec![Delta::PaneRemoved {
                pane,
                reason: RemovalReason::Closed,
            }];
            if let Some(arranged) = held.layout.clone().remove_leaf(pane) {
                deltas.push(Delta::LayoutChanged {
                    tab,
                    layout: arranged,
                });
            } else {
                cascaded = cascaded.saturating_add(1);
                deltas.push(Delta::TabRemoved { tab });
                if model
                    .sessions
                    .first()
                    .is_some_and(|found| found.tabs.len() == 1)
                {
                    deltas.push(Delta::SessionRemoved { session });
                }
            }
            let mut view = HostView::of(model.clone());
            let submission = submit(&mut view, SessionCommand::ClosePane { pane }, moment());
            assert!(
                submission.optimistic,
                "model {index}: a close shows at once"
            );
            assert!(
                same_but_for_the_generation(&view.model, &as_the_host_would(&model, &deltas)?),
                "model {index}: closing a pane showed something other than {deltas:?}"
            );
            assert_eq!(
                view.model.validate(),
                Ok(()),
                "model {index}: and what it showed is a model a host could hold"
            );
        }
        assert!(
            cascaded > 0,
            "some of the generated models had a tab of one pane, so the cascade was walked"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a command whose effect only the host can know is guessed at.
#[test]
fn optimistic_commands_wait_for_what_only_the_host_can_mint() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let model = generator.model();
        let tab = a_tab(&model)?;
        let pane = a_pane(&model)?;
        // Creation needs an identity, and moving or arranging needs the
        // layout the host will normalize: a placeholder invented here would be
        // reconciled away a moment later, which is more flicker than waiting.
        let waiting = [
            SessionCommand::CreateSession {
                name: "new".to_owned(),
                columns: 80,
                rows: 24,
                working_directory: None,
            },
            SessionCommand::CreateTab {
                session: a_session(&model)?,
                name: "new".to_owned(),
                columns: 80,
                rows: 24,
                working_directory: None,
            },
            SessionCommand::MovePane {
                pane,
                to_tab: tab,
                placement: Placement {
                    target: pane,
                    direction: SplitDirection::Horizontal,
                    before: false,
                },
            },
            SessionCommand::SetLayout {
                tab,
                layout: LayoutNode::Leaf(pane),
            },
        ];
        for command in waiting {
            let mut view = HostView::of(model.clone());
            let submission = submit(&mut view, command.clone(), moment());
            assert!(
                !submission.optimistic,
                "{command:?} waits for the host to say what it did"
            );
            assert_eq!(
                view.model, model,
                "and the screen does not move: {command:?}"
            );
            assert_eq!(
                view.pending.len(),
                1,
                "but it is still waiting for an answer"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a refusal does not put back exactly what it showed.
#[test]
fn optimistic_commands_put_back_what_the_host_refuses() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let model = generator.model();
        let session = a_session(&model)?;
        let mut view = HostView::of(model.clone());
        let submission = submit(
            &mut view,
            SessionCommand::RenameSession {
                session,
                name: RENAMED.to_owned(),
            },
            moment(),
        );
        assert_ne!(view.model, model, "it showed at once");
        assert_eq!(
            confirm(&mut view, submission.id, &rejected()),
            Confirmed::RolledBack,
            "and the refusal puts it back"
        );
        assert_eq!(view.model, model, "exactly as it was");
        assert!(view.pending.is_empty(), "with nothing left pending");
        // An answer to a command nobody here is waiting for changes nothing.
        assert_eq!(
            confirm(&mut view, CommandId(99), &rejected()),
            Confirmed::Unknown,
            "and an answer to a stranger is not acted on"
        );
        assert_eq!(view.model, model, "nor does it move anything");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a refusal of the first of two overlapping commands undoes the second
/// as well.
#[test]
fn optimistic_commands_undo_one_thing_at_a_time() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let model = generator.model();
        let tab = a_tab(&model)?;
        let mut view = HostView::of(model.clone());
        // Two renames of one tab, in flight together: the second was applied
        // on top of the first.
        let first = submit(
            &mut view,
            SessionCommand::RenameTab {
                tab,
                name: RENAMED.to_owned(),
            },
            moment(),
        );
        let second = submit(
            &mut view,
            SessionCommand::RenameTab {
                tab,
                name: AGAIN.to_owned(),
            },
            moment(),
        );
        assert_ne!(first.id, second.id, "each command has a number of its own");
        let named = |held: &HostView| -> Option<String> {
            held.model
                .sessions
                .iter()
                .flat_map(|session| session.tabs.iter())
                .find(|held| held.id == tab)
                .map(|held| held.name.clone())
        };
        assert_eq!(named(&view).as_deref(), Some(AGAIN), "the later one shows");
        // The host refuses the first. Only its effect goes; the second was
        // never refused and stands.
        assert_eq!(
            confirm(&mut view, first.id, &rejected()),
            Confirmed::RolledBack,
            "the first is put back"
        );
        assert_eq!(
            named(&view).as_deref(),
            Some(AGAIN),
            "and the second still shows, because one refusal undoes one thing"
        );
        assert_eq!(view.pending.len(), 1, "with the second still waiting");
        // And when the second is refused in its turn, the model is where it
        // began.
        assert_eq!(
            confirm(&mut view, second.id, &rejected()),
            Confirmed::RolledBack,
            "the second is put back too"
        );
        assert_eq!(view.model, model, "and nothing of either remains");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a command that is never answered is left showing for ever, or one
/// inside its time is given up on.
#[test]
fn optimistic_commands_give_up_on_what_is_never_answered() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let model = generator.model();
        let session = a_session(&model)?;
        let start = moment();
        let mut view = HostView::of(model.clone());
        let submission = submit(
            &mut view,
            SessionCommand::RenameSession {
                session,
                name: RENAMED.to_owned(),
            },
            start,
        );
        // Inside its time, nothing happens: the answer may still be coming.
        let soon = start
            .checked_add(PENDING_COMMAND_TIMEOUT)
            .ok_or("this machine's clock cannot reach the timeout")?;
        assert!(
            expire(&mut view, &work(), soon, PENDING_COMMAND_TIMEOUT).is_empty(),
            "a command inside its time is left alone"
        );
        assert_ne!(view.model, model, "and what it showed is still showing");
        // Past it, what it showed is put back and somebody is told, because a
        // screen in a state the host never agreed to is worse than a failure.
        let late = soon
            .checked_add(Duration::from_millis(1))
            .ok_or("this machine's clock cannot reach the timeout")?;
        assert_eq!(
            expire(&mut view, &work(), late, PENDING_COMMAND_TIMEOUT),
            vec![Notification::CommandTimedOut {
                host: work(),
                command: submission.id,
            }],
            "it is given up on by name"
        );
        assert_eq!(view.model, model, "and the screen is put back");
        assert!(view.pending.is_empty(), "with nothing left waiting");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a command number is given to two commands.
#[test]
fn optimistic_commands_never_give_one_number_twice() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let model = generator.model();
        let session = a_session(&model)?;
        let mut view = HostView::of(model);
        let mut given: Vec<Submission> = Vec::new();
        for _sent in 0..3_usize {
            let submission = submit(
                &mut view,
                SessionCommand::RenameSession {
                    session,
                    name: RENAMED.to_owned(),
                },
                moment(),
            );
            // Retired at once, so nothing is left to derive a number from: an
            // answer to a command given up on must not confirm a later one.
            let _settled = confirm(&mut view, submission.id, &applied());
            given.push(submission);
        }
        let numbers: Vec<u64> = given.iter().map(|held| held.id.0).collect();
        assert_eq!(
            numbers,
            vec![1, 2, 3],
            "each number is given once: {numbers:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the host announcing the change a command asked for disturbs what the
/// command already showed.
#[test]
fn optimistic_commands_survive_the_host_saying_the_same_thing() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let model = generator.model();
        let session = a_session(&model)?;
        let tab = a_tab(&model)?;
        let pane = a_pane(&model)?;
        let Some(held) = model
            .sessions
            .first()
            .and_then(|found| found.tabs.first())
            .cloned()
        else {
            return Err("the generated model has no tab".into());
        };
        let mut closing = vec![Delta::PaneRemoved {
            pane,
            reason: RemovalReason::Closed,
        }];
        if let Some(arranged) = held.layout.clone().remove_leaf(pane) {
            closing.push(Delta::LayoutChanged {
                tab,
                layout: arranged,
            });
        } else {
            closing.push(Delta::TabRemoved { tab });
        }
        // Every optimistic command, and the change the host announces for it.
        // The host announces it to the client that asked as well as to the
        // others, so what a command showed must survive its own delta.
        let table: Vec<(SessionCommand, Vec<Delta>)> = vec![
            (
                SessionCommand::RenameSession {
                    session,
                    name: RENAMED.to_owned(),
                },
                vec![Delta::SessionRenamed {
                    session,
                    name: RENAMED.to_owned(),
                }],
            ),
            (
                SessionCommand::CloseSession { session },
                vec![Delta::SessionRemoved { session }],
            ),
            (SessionCommand::ClosePane { pane }, closing),
        ];
        for (command, deltas) in table {
            // Both orders. The one the server actually uses is the answer
            // first — it replies to the command it applied and only then
            // pumps the delta out — and that is the order that used to leave
            // the delta unapplicable, because retiring the command took the
            // model it was applied to with it.
            for answer_first in [true, false] {
                walk(&model, &command, &deltas, answer_first)?;
            }
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Submits `command`, delivers the host's own `deltas` and its answer in the
/// order `answer_first` says, and holds the view to what it showed.
///
/// # Errors
///
/// When the host is not known or a delta cannot be encoded.
///
/// # Panics
///
/// When what the command showed does not survive the host saying the same
/// thing, in either order.
fn walk(
    model: &HostModel,
    command: &SessionCommand,
    deltas: &[Delta],
    answer_first: bool,
) -> Result<(), Failed> {
    let host = work();
    let mut client = ClientModel::default();
    let _first = client.insert(host.clone(), HostView::of(model.clone()));
    let Some(view) = client.host_mut(&host) else {
        return Err("the host is known".into());
    };
    let submission = submit(view, command.clone(), moment());
    let shown = view.model.clone();
    if answer_first {
        let _settled = confirm(view, submission.id, &applied());
    }
    for delta in deltas {
        let Some(standing) = client.host(&host) else {
            return Err("the host is known".into());
        };
        let at = standing.settled.generation;
        let taken = reduce(
            &mut client,
            &host,
            &ToClient::Delta {
                generation: Generation(at.0.saturating_add(1)),
                payload: encode_delta(delta)?,
            },
        );
        assert!(
            taken.is_empty(),
            "{command:?} ({answer_first}): the host's own change asked for nothing: {taken:?}"
        );
    }
    let Some(after) = client.host_mut(&host) else {
        return Err("the host is known".into());
    };
    assert!(
        same_but_for_the_generation(&after.model, &shown),
        "{command:?} ({answer_first}): what was shown is still shown once the host says it"
    );
    if !answer_first {
        assert_eq!(
            confirm(after, submission.id, &applied()),
            Confirmed::Applied,
            "and the answer retires it"
        );
    }
    assert!(
        same_but_for_the_generation(&after.model, &as_the_host_would(model, deltas)?),
        "{command:?} ({answer_first}): and the model is the host's own"
    );
    Ok(())
}
