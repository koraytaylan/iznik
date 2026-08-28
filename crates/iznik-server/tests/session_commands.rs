//! A command answered exactly once, and a refusal that leaves no trace: not
//! the generation, not a pane, not a delta. A half-applied command is the one
//! thing a client cannot reconcile, so every refusal is checked against all
//! three.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iznik_protocol::command::{CommandOutcome, Created, Placement, RejectionCode, SessionCommand};
use iznik_protocol::delta::Delta;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, SplitDirection, Weighted};
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::commands::apply;
use iznik_server::session::registry::{Numbered, Registry, RegistryDefaults};
use iznik_server::terminal::mirror::{MirrorError, MirrorThread};
use tokio::sync::broadcast;

/// The size every pane in these cases is created at.
const COLUMNS: u16 = 80;

/// The height every pane in these cases is created at.
const ROWS: u16 = 24;

/// How many renames the exactly-once case sends.
const RENAMES: usize = 1_000;

/// How long a thousand renames may take, which is the bar an in-process test
/// is held to for work that starts no process.
const RENAME_BUDGET: Duration = Duration::from_secs(1);

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a case waits for a shell to answer, in short looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How many looks a case takes before it gives up on a shell.
const POLL_ATTEMPTS: usize = 250;

/// A registry whose panes run the given program.
///
/// # Errors
///
/// When the mirror thread cannot be started.
fn registry_running(program: Program) -> Result<Registry, MirrorError> {
    let mirrors = MirrorThread::start()?;
    let budget = Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES)));
    Ok(Registry::new(
        RegistryDefaults {
            program,
            terminfo_directory: None,
        },
        budget,
        mirrors,
    ))
}

/// A registry whose panes run `sh`.
///
/// # Errors
///
/// When the mirror thread cannot be started.
fn registry() -> Result<Registry, MirrorError> {
    registry_running(Program::Command {
        path: "sh".into(),
        arguments: Vec::new(),
    })
}

/// A command that makes a session.
fn create_session(name: &str) -> SessionCommand {
    SessionCommand::CreateSession {
        name: name.to_owned(),
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
    }
}

/// Everything the registry has said that the receiver has not taken.
fn drain(receiver: &mut broadcast::Receiver<Numbered<Delta>>) -> Vec<Numbered<Delta>> {
    let mut seen = Vec::new();
    while let Ok(numbered) = receiver.try_recv() {
        seen.push(numbered);
    }
    seen
}

/// The first pane and the tab holding it.
fn first_pane(model: &HostModel) -> Option<(TabId, PaneId)> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter().map(|pane| (tab.id, pane.id)))
        .next()
}

/// What a command answered it made.
///
/// # Errors
///
/// When it was refused, with the code and the words it gave.
fn made(outcome: &CommandOutcome) -> Result<Created, String> {
    match outcome {
        CommandOutcome::Applied { created, .. } => Ok(*created),
        CommandOutcome::Rejected { code, message } => {
            Err(format!("a command was refused as {code:?}: {message}"))
        }
    }
}

/// Each of the eleven commands is applied, answers what it made, and the model
/// shows it.
///
/// # Panics
///
/// When a command is refused, or answers something it did not make.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_commands_every_command_is_applied_and_answered() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let session =
            match made(&apply(&mut registry, create_session("work")).await).expect("a session") {
                Created::Session(session) => session,
                other => panic!("a session was made, not {other:?}"),
            };
        let tab = match made(
            &apply(
                &mut registry,
                SessionCommand::CreateTab {
                    session,
                    name: "edit".to_owned(),
                    columns: COLUMNS,
                    rows: ROWS,
                    working_directory: None,
                },
            )
            .await,
        )
        .expect("a tab")
        {
            Created::Tab(made_tab) => made_tab,
            other => panic!("a tab was made, not {other:?}"),
        };
        // The placement names a pane of the tab being split, not just any
        // pane the host holds.
        let target = second_pane(&registry.snapshot(), tab).expect("the new tab's pane");
        let pane = match made(
            &apply(
                &mut registry,
                SessionCommand::CreatePane {
                    tab,
                    placement: Placement {
                        target,
                        direction: SplitDirection::Horizontal,
                        before: false,
                    },
                    columns: COLUMNS,
                    rows: ROWS,
                    working_directory: None,
                },
            )
            .await,
        )
        .expect("a pane")
        {
            Created::Pane(made_pane) => made_pane,
            other => panic!("a pane was made, not {other:?}"),
        };
        // The eight that change what is there answer `Nothing`, because there
        // is nothing new for a client to be told the id of.
        let second_tab = match made(
            &apply(
                &mut registry,
                SessionCommand::CreateTab {
                    session,
                    name: "second".to_owned(),
                    columns: COLUMNS,
                    rows: ROWS,
                    working_directory: None,
                },
            )
            .await,
        )
        .expect("a tab")
        {
            Created::Tab(made_tab) => made_tab,
            other => panic!("a tab was made, not {other:?}"),
        };
        // The whole order, which is every tab the session holds — the one the
        // session was made with, and the two added since.
        let mut order: Vec<TabId> = registry
            .snapshot()
            .sessions
            .iter()
            .find(|held| held.id == session)
            .map(|held| held.tabs.iter().map(|found| found.id).collect())
            .unwrap_or_default();
        order.reverse();
        assert_eq!(order.len(), 3, "the session holds three tabs");
        let move_target = second_pane(&registry.snapshot(), second_tab).expect("a pane");
        for change in changes(session, tab, second_tab, target, pane, order, move_target) {
            let named = format!("{change:?}");
            let before = registry.generation();
            let outcome = apply(&mut registry, change).await;
            assert_eq!(made(&outcome).expect(&named), Created::Nothing, "{named}");
            match outcome {
                CommandOutcome::Applied { generation, .. } => {
                    assert!(generation > before, "{named} advanced no generation");
                    assert_eq!(generation, registry.generation(), "{named} answered stale");
                }
                CommandOutcome::Rejected { .. } => panic!("{named} was refused"),
            }
            registry.snapshot().validate().expect("it holds together");
        }
        assert_eq!(registry.snapshot().sessions.len(), 0, "nothing is left");
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the happy path finishes");
}

/// The eight commands that change what is already there, in the order this
/// case applies them: each leaves the model whole for the next.
fn changes(
    session: SessionId,
    tab: TabId,
    second_tab: TabId,
    target: PaneId,
    pane: PaneId,
    order: Vec<TabId>,
    move_target: PaneId,
) -> Vec<SessionCommand> {
    vec![
        SessionCommand::RenameSession {
            session,
            name: "renamed".to_owned(),
        },
        SessionCommand::RenameTab {
            tab,
            name: "retitled".to_owned(),
        },
        SessionCommand::ReorderTabs { session, order },
        SessionCommand::SetLayout {
            tab,
            layout: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    Weighted {
                        node: LayoutNode::Leaf(target),
                        weight: 1,
                    },
                    Weighted {
                        node: LayoutNode::Leaf(pane),
                        weight: 1,
                    },
                ],
            },
        },
        SessionCommand::MovePane {
            pane,
            to_tab: second_tab,
            placement: Placement {
                target: move_target,
                direction: SplitDirection::Vertical,
                before: true,
            },
        },
        SessionCommand::ClosePane { pane },
        SessionCommand::CloseTab { tab },
        SessionCommand::CloseSession { session },
    ]
}

/// A pane of a tab other than the one the first pane is in.
fn second_pane(model: &HostModel, tab: TabId) -> Option<PaneId> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .find(|held| held.id == tab)
        .and_then(|held| held.panes.first())
        .map(|pane| pane.id)
}

/// Every rejection code is given by the command that earns it, and a refusal
/// leaves the generation and the delta stream exactly as they were.
///
/// # Panics
///
/// When a command is accepted, refused by another code, or changes anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_commands_every_refusal_leaves_no_trace() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let session =
            match made(&apply(&mut registry, create_session("work")).await).expect("a session") {
                Created::Session(session) => session,
                other => panic!("a session was made, not {other:?}"),
            };
        let (tab, pane) = first_pane(&registry.snapshot()).expect("a pane");
        let missing_session = SessionId(u64::MAX);
        let missing_tab = TabId(u64::MAX);
        let missing_pane = PaneId(u64::MAX);
        let refused = vec![
            (
                SessionCommand::RenameSession {
                    session: missing_session,
                    name: "away".to_owned(),
                },
                RejectionCode::UnknownSession,
            ),
            (
                SessionCommand::RenameTab {
                    tab: missing_tab,
                    name: "away".to_owned(),
                },
                RejectionCode::UnknownTab,
            ),
            (
                SessionCommand::ClosePane { pane: missing_pane },
                RejectionCode::UnknownPane,
            ),
            (
                SessionCommand::RenameSession {
                    session,
                    name: String::new(),
                },
                RejectionCode::EmptyName,
            ),
            (
                SessionCommand::ReorderTabs {
                    session,
                    order: vec![tab, tab],
                },
                RejectionCode::InvalidOrder,
            ),
            (
                SessionCommand::SetLayout {
                    tab,
                    layout: LayoutNode::Leaf(missing_pane),
                },
                RejectionCode::InvalidLayout,
            ),
            (
                SessionCommand::CreateTab {
                    session,
                    name: "nowhere".to_owned(),
                    columns: COLUMNS,
                    rows: ROWS,
                    working_directory: Some("/nowhere/at/all".to_owned()),
                },
                RejectionCode::SpawnFailed,
            ),
        ];
        let mut deltas = registry.deltas();
        for (command, expected) in refused {
            let named = format!("{command:?}");
            let before = registry.snapshot();
            match apply(&mut registry, command).await {
                CommandOutcome::Rejected { code, message } => {
                    assert_eq!(code, expected, "{named} said {message}");
                    assert!(!message.is_empty(), "{named} refused without saying why");
                }
                CommandOutcome::Applied { .. } => panic!("{named} was applied"),
            }
            assert_eq!(registry.snapshot(), before, "{named} changed the model");
            assert!(drain(&mut deltas).is_empty(), "{named} said something");
        }
        assert_eq!(
            first_pane(&registry.snapshot()),
            Some((tab, pane)),
            "the pane that was there still is"
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the refusal case finishes");
}

/// A pane whose program does not exist is refused as a spawn failure, with
/// what the pseudoterminal said, and nothing is added.
///
/// # Panics
///
/// When it is refused by another code, or anything is added.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_commands_a_spawn_failure_is_reported() {
    let case = async {
        let mut registry = registry_running(Program::Command {
            path: "/nonexistent".into(),
            arguments: Vec::new(),
        })
        .expect("a registry");
        let mut deltas = registry.deltas();
        match apply(&mut registry, create_session("work")).await {
            CommandOutcome::Rejected { code, message } => {
                assert_eq!(code, RejectionCode::SpawnFailed, "{message}");
                assert!(!message.is_empty(), "a spawn failure said nothing");
            }
            CommandOutcome::Applied { .. } => panic!("a pane with no program was started"),
        }
        assert_eq!(registry.snapshot().sessions.len(), 0, "nothing was added");
        assert!(drain(&mut deltas).is_empty(), "a failure said something");
        assert_eq!(registry.generation(), Generation(0), "and moved nothing");
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the spawn-failure case finishes");
}

/// A thousand commands produce exactly a thousand answers, in order.
///
/// # Panics
///
/// When one is answered twice, not at all, or out of order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_commands_are_answered_exactly_once() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let session =
            match made(&apply(&mut registry, create_session("work")).await).expect("a session") {
                Created::Session(session) => session,
                other => panic!("a session was made, not {other:?}"),
            };
        let began = Instant::now();
        let mut answers = Vec::with_capacity(RENAMES);
        for round in 0..RENAMES {
            answers.push(
                apply(
                    &mut registry,
                    SessionCommand::RenameSession {
                        session,
                        name: format!("work-{round}"),
                    },
                )
                .await,
            );
        }
        let took = began.elapsed();
        assert_eq!(answers.len(), RENAMES, "one answer per command");
        let generations: Vec<u64> = answers
            .iter()
            .map(|outcome| match outcome {
                CommandOutcome::Applied { generation, .. } => generation.0,
                CommandOutcome::Rejected { .. } => 0,
            })
            .collect();
        let ordered: Vec<u64> = generations
            .windows(2)
            .filter(|pair| {
                pair.first().copied().unwrap_or(0).saturating_add(1)
                    == pair.last().copied().unwrap_or(0)
            })
            .map(|pair| pair.first().copied().unwrap_or(0))
            .collect();
        assert_eq!(
            ordered.len(),
            RENAMES.saturating_sub(1),
            "the answers were not one generation apart, in order"
        );
        assert!(took <= RENAME_BUDGET, "{RENAMES} renames took {took:?}");
        assert_eq!(
            registry
                .snapshot()
                .sessions
                .first()
                .map(|held| held.name.clone()),
            Some(format!("work-{}", RENAMES.saturating_sub(1))),
            "the last rename is the one that stands"
        );
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the exactly-once case finishes");
}

/// A pane created with a size and a directory has a shell that reports them.
///
/// # Panics
///
/// When the shell reports something else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_commands_a_pane_starts_where_and_how_it_was_asked() {
    let case = async {
        let mut registry = registry().expect("a registry");
        let outcome = apply(
            &mut registry,
            SessionCommand::CreateSession {
                name: "work".to_owned(),
                columns: 100,
                rows: 37,
                working_directory: Some("/tmp".to_owned()),
            },
        )
        .await;
        assert!(
            matches!(made(&outcome).expect("a session"), Created::Session(_named)),
            "a session"
        );
        let (_tab, pane) = first_pane(&registry.snapshot()).expect("a pane");
        let held = registry.pane(pane).expect("the pane");
        held.input(
            b"stty size; pwd
"
            .to_vec(),
        )
        .expect("the question is asked");
        let mut attempts: usize = 0;
        let answer = loop {
            let seen = held
                .read_history(iznik_protocol::identity::Sequence(0))
                .unwrap_or_default();
            let text = String::from_utf8_lossy(&seen).into_owned();
            if text.contains("37 100") && text.contains("/tmp") {
                break text;
            }
            attempts = attempts.saturating_add(1);
            assert!(attempts <= POLL_ATTEMPTS, "the shell said {text:?}");
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert!(answer.contains("37 100"), "the size it was asked for");
        assert!(answer.contains("/tmp"), "the directory it was asked for");
        let _closed = registry.close_pane(pane);
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the spawn-parameters case finishes");
}
