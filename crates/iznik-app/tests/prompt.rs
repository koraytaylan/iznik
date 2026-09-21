//! The palette's argument step: which actions ask, what they offer, and the
//! command each answer becomes.

use iznik_app::actions::ActionId;
use iznik_app::bridge::EngineEvent;
use iznik_app::host_ui::EngineState;
use iznik_app::prompt::{
    Answer, HostOperation, Prompt, Step, answer, begin, choices, numbered_name,
};
use iznik_app::window::TabKey;
use iznik_client::bootstrap::probe::InstalledServer;
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::{HostState, UpgradeOffer, UpgradeReason};
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{Placement, SessionCommand};
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{
    HostModel, LayoutNode, Pane, Session, SplitDirection, Tab, Weighted, encode_host_model,
};

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// The host every fixture model belongs to.
fn host() -> HostId {
    HostId("build".to_owned())
}

/// A pane fixture with the given id.
fn pane(id: u64) -> Pane {
    Pane {
        id: PaneId(id),
        title: String::new(),
        working_directory: None,
        columns: 80,
        rows: 24,
    }
}

/// One session holding three tabs: two panes side by side, then one pane
/// each.
fn model() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![
                Tab {
                    id: TabId(10),
                    name: "editor".to_owned(),
                    panes: vec![pane(100), pane(101)],
                    layout: LayoutNode::Split {
                        direction: SplitDirection::Horizontal,
                        children: vec![
                            Weighted {
                                node: LayoutNode::Leaf(PaneId(100)),
                                weight: 3,
                            },
                            Weighted {
                                node: LayoutNode::Leaf(PaneId(101)),
                                weight: 1,
                            },
                        ],
                    },
                },
                Tab {
                    id: TabId(11),
                    name: "logs".to_owned(),
                    panes: vec![pane(110)],
                    layout: LayoutNode::Leaf(PaneId(110)),
                },
                Tab {
                    id: TabId(12),
                    name: "build".to_owned(),
                    panes: vec![pane(120)],
                    layout: LayoutNode::Leaf(PaneId(120)),
                },
            ],
        }],
    }
}

/// An engine state holding the fixture model.
///
/// # Errors
///
/// Returns the encoding failure when the fixture model does not encode.
fn state() -> Result<EngineState, Failed> {
    let mut state = EngineState::new();
    let model = model();
    state.apply(
        &host(),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    );
    Ok(state)
}

/// The selection of one fixture tab.
fn selected(tab: u64) -> TabKey {
    TabKey {
        host: host(),
        session: SessionId(1),
        tab: TabId(tab),
    }
}

/// The prompt an action opens over the fixture with a tab selected.
///
/// # Errors
///
/// Returns a failure when the fixture does not encode or the action opens no prompt.
fn prompt(action: ActionId, tab: u64) -> Result<Prompt, Failed> {
    asked(begin(action, &state()?, Some(&selected(tab))))
}

/// The prompt a step asks, when it asks one.
///
/// # Errors
///
/// Returns a failure when the step performs at once or does not exist.
fn asked(step: Option<Step>) -> Result<Prompt, Failed> {
    match step {
        Some(Step::Ask(prompt)) => Ok(prompt),
        other => Err(format!("expected a prompt, got {other:?}").into()),
    }
}

/// Tell the fixture state that hosts are in the given connection states.
fn moved(state: &mut EngineState, hosts: &[(&str, HostState)]) {
    for (alias, connection) in hosts {
        state.absorb(EngineEvent::Said(ManagerEvent::Moved {
            host: HostId((*alias).to_owned()),
            state: connection.clone(),
        }));
    }
}

/// A connection state carrying a failure.
fn failed() -> HostState {
    HostState::Failed {
        error: "unreachable".to_owned(),
        retry_at: std::time::Instant::now(),
    }
}

/// A connected state with no upgrade on offer.
fn connected() -> HostState {
    HostState::Connected {
        server_version: "0.0.0".to_owned(),
        capabilities: Capabilities::REORDER_SESSIONS,
        upgrade: None,
    }
}

/// The command an answered prompt sends.
///
/// # Errors
///
/// Returns a failure when the answer is refused, is not a session command, or
/// is for another host.
fn command(prompt: &Prompt, text: &str, index: usize) -> Result<SessionCommand, Failed> {
    match answer(prompt, text, index) {
        Some(Answer::Command {
            host: target,
            command,
        }) if target == host() => Ok(command),
        other => Err(format!("expected a command for the fixture host, got {other:?}").into()),
    }
}

#[test]
/// Only the actions that need an argument open a prompt.
///
/// # Panics
///
/// Panics when an argument-free action asks, or an argument action does not.
fn only_argument_actions_open_a_prompt() {
    let state = state().expect("fixture state");
    let key = selected(10);
    for action in [
        ActionId::CreateSession,
        ActionId::CloseSession,
        ActionId::CreateTab,
        ActionId::CloseTab,
        ActionId::CreatePane,
        ActionId::ClosePane,
        ActionId::RemoveHost,
        ActionId::ReconnectHost,
        ActionId::UpgradeHost,
        ActionId::UninstallHost,
        ActionId::OpenSettings,
    ] {
        assert!(begin(action, &state, Some(&key)).is_none(), "{action:?}");
    }
    for action in [
        ActionId::AddHost,
        ActionId::RenameSession,
        ActionId::RenameTab,
        ActionId::ReorderTabs,
        ActionId::MovePane,
        ActionId::SetLayout,
    ] {
        assert!(begin(action, &state, Some(&key)).is_some(), "{action:?}");
    }
    assert!(begin(ActionId::RenameTab, &state, None).is_none());
    assert!(begin(ActionId::AddHost, &EngineState::new(), None).is_some());
}

#[test]
/// Add Host sends the trimmed alias and refuses a blank one.
///
/// # Panics
///
/// Panics when the alias answer differs.
fn add_host_answers_with_the_trimmed_alias() {
    let prompt = asked(begin(ActionId::AddHost, &EngineState::new(), None)).expect("add host asks");
    assert_eq!(
        answer(&prompt, "  devbox ", 0),
        Some(Answer::AddHost("devbox".to_owned()))
    );
    assert_eq!(answer(&prompt, "   ", 0), None);
}

#[test]
/// Renames start from the current name and send the new one.
///
/// # Panics
///
/// Panics when a rename prompt or its command differs.
fn renames_start_from_the_current_name() {
    let session = prompt(ActionId::RenameSession, 11).expect("prompt opens");
    assert_eq!(session.initial, "work");
    assert_eq!(
        command(&session, " play ", 0).expect("answer sends a command"),
        SessionCommand::RenameSession {
            session: SessionId(1),
            name: "play".to_owned(),
        }
    );
    let tab = prompt(ActionId::RenameTab, 11).expect("prompt opens");
    assert_eq!(tab.initial, "logs");
    assert_eq!(
        command(&tab, "tail", 0).expect("answer sends a command"),
        SessionCommand::RenameTab {
            tab: TabId(11),
            name: "tail".to_owned(),
        }
    );
    assert_eq!(answer(&tab, "", 0), None);
}

#[test]
/// Reordering offers every other position as a whole order.
///
/// # Panics
///
/// Panics when the offered positions or orders differ.
fn reorder_offers_every_other_position_as_a_whole_order() {
    let prompt = prompt(ActionId::ReorderTabs, 11).expect("prompt opens");
    let labels: Vec<_> = choices(&prompt, "")
        .iter()
        .map(|choice| choice.label.clone())
        .collect();
    assert_eq!(
        labels,
        [
            "before \u{201C}editor\u{201D}",
            "after \u{201C}build\u{201D}"
        ]
    );
    assert_eq!(
        command(&prompt, "", 1).expect("answer sends a command"),
        SessionCommand::ReorderTabs {
            session: SessionId(1),
            order: vec![TabId(10), TabId(12), TabId(11)],
        }
    );
}

#[test]
/// Reordering sessions offers every other position of the selected session as
/// a whole order of the host's sessions.
///
/// # Panics
///
/// Panics when the offered positions or orders differ.
fn reorder_sessions_offers_every_other_position_as_a_whole_order() {
    let mut state = EngineState::new();
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![session(1, "work"), session(2, "play"), session(3, "logs")],
    };
    state.apply(
        &host(),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model).expect("model encodes"),
        },
    );
    // Only a server that advertised it can reorder sessions offers it.
    moved(&mut state, &[("build", connected())]);
    let key = TabKey {
        host: host(),
        session: SessionId(2),
        tab: TabId(10),
    };
    let prompt = asked(begin(ActionId::ReorderSessions, &state, Some(&key))).expect("prompt opens");
    let labels: Vec<_> = choices(&prompt, "")
        .iter()
        .map(|choice| choice.label.clone())
        .collect();
    assert_eq!(
        labels,
        ["before \u{201C}work\u{201D}", "after \u{201C}logs\u{201D}"]
    );
    assert_eq!(
        command(&prompt, "", 0).expect("answer sends a command"),
        SessionCommand::ReorderSessions {
            order: vec![SessionId(2), SessionId(1), SessionId(3)],
        }
    );
    assert_eq!(
        command(&prompt, "", 1).expect("answer sends a command"),
        SessionCommand::ReorderSessions {
            order: vec![SessionId(1), SessionId(3), SessionId(2)],
        }
    );
}

#[test]
/// A host holding one session has no other position to offer.
///
/// # Panics
///
/// Panics when a single-session host opens a reorder prompt.
fn reorder_sessions_needs_somewhere_to_move() {
    assert!(begin(ActionId::ReorderSessions, &state().expect("fixture"), None).is_none());
}

#[test]
/// A connected server that did not advertise it can reorder sessions is never
/// asked to: the prompt does not open, so no command is built for it.
///
/// # Panics
///
/// Panics when a host whose server cannot decode the command offers it.
fn reorder_sessions_is_not_offered_to_a_server_that_cannot_decode_it() {
    let mut state = EngineState::new();
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![session(1, "work"), session(2, "play")],
    };
    state.apply(
        &host(),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model).expect("model encodes"),
        },
    );
    // A server of a build that predates `ReorderSessions` advertises neither
    // compression nor resume nor the reorder; the connection carries what it
    // said and nothing more.
    state.absorb(EngineEvent::Said(ManagerEvent::Moved {
        host: host(),
        state: HostState::Connected {
            server_version: "0.0.0".to_owned(),
            capabilities: Capabilities::ZSTD,
            upgrade: None,
        },
    }));
    assert!(begin(ActionId::ReorderSessions, &state, None).is_none());
}

/// One session holding one tab and one pane, named as given.
fn session(id: u64, name: &str) -> Session {
    Session {
        id: SessionId(id),
        name: name.to_owned(),
        tabs: vec![Tab {
            id: TabId(10),
            name: "shell".to_owned(),
            panes: vec![pane(100)],
            layout: LayoutNode::Leaf(PaneId(100)),
        }],
    }
}

#[test]
/// Moving a pane offers every other tab and places it beside that tab's last pane.
///
/// # Panics
///
/// Panics when the destinations or the placement differ.
fn move_pane_offers_every_other_tab() {
    let prompt = prompt(ActionId::MovePane, 10).expect("prompt opens");
    let labels: Vec<_> = choices(&prompt, "")
        .iter()
        .map(|choice| choice.label.clone())
        .collect();
    assert_eq!(labels, ["work / logs", "work / build"]);
    assert_eq!(
        command(&prompt, "build", 0).expect("answer sends a command"),
        SessionCommand::MovePane {
            pane: PaneId(100),
            to_tab: TabId(12),
            placement: Placement {
                target: PaneId(120),
                direction: SplitDirection::Horizontal,
                before: false,
            },
        }
    );
}

#[test]
/// Arrangements lay the panes out evenly in reading order; one pane has none.
///
/// # Panics
///
/// Panics when an arrangement differs.
fn set_layout_offers_even_arrangements() {
    let prompt = prompt(ActionId::SetLayout, 10).expect("prompt opens");
    assert_eq!(
        command(&prompt, "stack", 0).expect("answer sends a command"),
        SessionCommand::SetLayout {
            tab: TabId(10),
            layout: LayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    Weighted {
                        node: LayoutNode::Leaf(PaneId(100)),
                        weight: 1,
                    },
                    Weighted {
                        node: LayoutNode::Leaf(PaneId(101)),
                        weight: 1,
                    },
                ],
            },
        }
    );
    assert!(
        choices(
            &self::prompt(ActionId::SetLayout, 11).expect("prompt opens"),
            ""
        )
        .is_empty()
    );
}

#[test]
/// A choice answer indexes the filtered choices, and one matching nothing sends nothing.
///
/// # Panics
///
/// Panics when filtering and selection disagree.
fn choice_answers_index_the_filtered_choices() {
    let prompt = prompt(ActionId::MovePane, 10).expect("prompt opens");
    assert_eq!(choices(&prompt, "logs").len(), 1);
    assert_eq!(answer(&prompt, "logs", 1), None);
    assert_eq!(answer(&prompt, "zzz", 0), None);
}

#[test]
/// A host operation with one applicable host performs at once on it.
///
/// # Panics
///
/// Panics when the operation asks, or acts on the wrong host.
fn a_host_operation_with_one_host_is_performed_at_once() {
    let mut state = EngineState::new();
    moved(&mut state, &[("devbox", connected()), ("broken", failed())]);
    assert_eq!(
        begin(ActionId::ReconnectHost, &state, None),
        Some(Step::Perform(Answer::Host {
            operation: HostOperation::Reconnect,
            host: HostId("broken".to_owned()),
        }))
    );
    assert_eq!(begin(ActionId::UpgradeHost, &state, None), None);
}

#[test]
/// With several applicable hosts a host operation asks which, the selected
/// tab's host first.
///
/// # Panics
///
/// Panics when the choice or its order differs.
fn a_host_operation_with_many_hosts_asks_which() {
    let mut state = state().expect("fixture state");
    moved(
        &mut state,
        &[("alpha", connected()), ("build", connected())],
    );
    let prompt = asked(begin(ActionId::RemoveHost, &state, Some(&selected(10)))).expect("asks");
    let labels: Vec<_> = choices(&prompt, "")
        .iter()
        .map(|choice| choice.label.clone())
        .collect();
    assert_eq!(labels, ["Remove build", "Remove alpha"]);
    assert_eq!(
        answer(&prompt, "alpha", 0),
        Some(Answer::Host {
            operation: HostOperation::Remove,
            host: HostId("alpha".to_owned()),
        })
    );
}

#[test]
/// Uninstalling asks for confirmation even with a single host.
///
/// # Panics
///
/// Panics when uninstalling performs without asking.
fn uninstall_asks_even_for_one_host() {
    let mut state = EngineState::new();
    moved(&mut state, &[("devbox", connected())]);
    let prompt = asked(begin(ActionId::UninstallHost, &state, None)).expect("uninstall asks");
    assert!(prompt.question.contains("ends every session"));
    assert_eq!(choices(&prompt, "").len(), 1);
}

#[test]
/// Upgrading asks even for one host, and the question says every session on
/// it ends — the daemon *is* the sessions, so that is never assumed.
///
/// # Panics
///
/// Panics when an upgrade performs without asking or does not warn.
fn upgrade_asks_even_for_one_host_and_warns() {
    let mut state = EngineState::new();
    let installed = InstalledServer {
        crate_version: "0.0.0".to_owned(),
        protocol_version: 1,
    };
    moved(
        &mut state,
        &[(
            "devbox",
            HostState::Connected {
                server_version: "0.0.0".to_owned(),
                capabilities: Capabilities::REORDER_SESSIONS,
                upgrade: Some(UpgradeOffer {
                    installed: installed.clone(),
                    bundled: InstalledServer {
                        crate_version: "0.2.0".to_owned(),
                        protocol_version: 2,
                    },
                    reason: UpgradeReason::Version,
                }),
            },
        )],
    );
    let prompt = asked(begin(ActionId::UpgradeHost, &state, None)).expect("upgrade asks");
    assert!(
        prompt.question.contains("ends every session"),
        "the question warns what an upgrade costs: {}",
        prompt.question
    );
    assert_eq!(choices(&prompt, "").len(), 1);
}

#[test]
/// A host whose connected server is missing a feature carries an upgrade
/// offer, and `Upgrade Host` is then offered for it — even at an unchanged
/// version.
///
/// # Panics
///
/// Panics when a same-version server missing the reorder raises no offer, or
/// the action is unavailable while it is.
fn a_capability_gap_offers_an_upgrade_and_the_action() {
    let mut state = EngineState::new();
    // A same-version server that advertised no capabilities: the version
    // cannot distinguish it, so the gap is what the offer is made of.
    moved(
        &mut state,
        &[(
            "devbox",
            HostState::Connected {
                server_version: "0.0.0".to_owned(),
                capabilities: Capabilities::from_bits(0),
                upgrade: Some(UpgradeOffer {
                    installed: InstalledServer {
                        crate_version: "0.0.0".to_owned(),
                        protocol_version: 1,
                    },
                    bundled: InstalledServer {
                        crate_version: "0.0.0".to_owned(),
                        protocol_version: 1,
                    },
                    reason: UpgradeReason::Capabilities,
                }),
            },
        )],
    );
    let host = HostId("devbox".to_owned());
    assert!(
        state.missing_capabilities_for(&host),
        "a server with no features is missing them"
    );
    assert_eq!(
        upgrade_action_available(&state),
        Some(true),
        "the upgrade action is offered for the gap"
    );

    // And a server of another version — the case a stale local build is —
    // offers it just the same, so the palette entry is reachable either way.
    let mut versioned = EngineState::new();
    moved(
        &mut versioned,
        &[(
            "devbox",
            HostState::Connected {
                server_version: "0.0.1".to_owned(),
                capabilities: Capabilities::from_bits(0),
                upgrade: Some(UpgradeOffer {
                    installed: InstalledServer {
                        crate_version: "0.0.1".to_owned(),
                        protocol_version: 1,
                    },
                    bundled: InstalledServer {
                        crate_version: "0.0.0".to_owned(),
                        protocol_version: 1,
                    },
                    reason: UpgradeReason::Version,
                }),
            },
        )],
    );
    assert_eq!(
        upgrade_action_available(&versioned),
        Some(true),
        "the upgrade action is offered for a version difference too"
    );
}

/// Whether the palette lists `host: upgrade` for a state, or `None` when the
/// inventory holds no such row.
fn upgrade_action_available(state: &EngineState) -> Option<bool> {
    iznik_app::actions::INVENTORY
        .iter()
        .find(|spec| spec.id == ActionId::UpgradeHost)
        .map(|spec| iznik_app::actions::available(spec, state))
}

#[test]
/// An offer for a capability gap and one for a version differ in what they
/// say, so a person can tell why they are being asked.
///
/// # Panics
///
/// Panics when the two reasons read the same.
fn an_upgrades_reason_is_said() {
    let installed = InstalledServer {
        crate_version: "0.0.0".to_owned(),
        protocol_version: 1,
    };
    let bundled = InstalledServer {
        crate_version: "0.2.0".to_owned(),
        protocol_version: 2,
    };
    let host = HostId("devbox".to_owned());
    let version = UpgradeOffer {
        installed: installed.clone(),
        bundled: bundled.clone(),
        reason: UpgradeReason::Version,
    };
    let gap = UpgradeOffer {
        installed,
        bundled,
        reason: UpgradeReason::Capabilities,
    };
    assert!(version.summary(&host).contains("0.2.0"));
    assert!(gap.summary(&host).contains("missing features"));
    assert_ne!(version.summary(&host), gap.summary(&host));
}

#[test]
/// New names take the first free number after the base.
///
/// # Panics
///
/// Panics when a numbered name differs.
fn numbered_names_pass_taken_ones() {
    assert_eq!(numbered_name("shell", ["logs"].into_iter()), "shell");
    assert_eq!(
        numbered_name("shell", ["shell", "shell 2"].into_iter()),
        "shell 3"
    );
}
