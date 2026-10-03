//! Closing a tab that is running programs asks first; a tab of idle shells
//! closes at once.

use std::time::Instant;

use iznik_app::actions::ActionId;
use iznik_app::close_ask::{CANCEL_LABEL, CloseAsks, DISMISS_HINT, question};
use iznik_app::host_ui::{EngineState, NoticeKind};
use iznik_app::prompt::{Answer, Step, begin, begin_asking};
use iznik_app::settings::Settings;
use iznik_app::window::TabKey;
use iznik_client::host::identity::HostId;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{CommandId, Generation, PaneId, SessionId, TabId};
use iznik_protocol::message::ToClient;
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab, encode_host_model};

/// A state holding one tab whose one pane's title is `title`.
///
/// # Errors
/// Returns the encoding failure when the fixture model does not encode.
fn state(title: &str) -> Result<EngineState, Box<dyn std::error::Error>> {
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(2),
                name: "shell".to_owned(),
                layout: LayoutNode::Leaf(PaneId(3)),
                panes: vec![Pane {
                    id: PaneId(3),
                    title: title.to_owned(),
                    working_directory: None,
                    columns: 80,
                    rows: 24,
                }],
            }],
        }],
    };
    let mut state = EngineState::new();
    state.apply(
        &HostId("build".to_owned()),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    );
    Ok(state)
}

/// The fixture's one tab.
fn key() -> TabKey {
    TabKey {
        host: HostId("build".to_owned()),
        session: SessionId(1),
        tab: TabId(2),
    }
}

/// A tab running `vim` asks, and confirming closes it; an idle shell does not ask.
///
/// # Panics
/// Panics when a running tab closes unasked or an idle one asks.
#[test]
fn closing_a_running_tab_asks_first() {
    let Some(Step::Confirm(asked)) = begin(
        ActionId::CloseTab,
        &state("vim").expect("fixture"),
        Some(&key()),
    ) else {
        panic!("a running tab asks");
    };
    assert_eq!(asked.title, "Close this tab?");
    assert!(asked.detail.contains("vim is running"), "{}", asked.detail);
    assert_eq!(asked.confirm, "Close Tab");
    assert_eq!(CANCEL_LABEL, "Cancel");
    assert!(DISMISS_HINT.contains("Escape cancels"), "{DISMISS_HINT}");
    assert_eq!(
        asked.answer,
        Answer::Command {
            host: key().host,
            command: SessionCommand::CloseTab { tab: TabId(2) },
        }
    );
    assert!(
        begin(
            ActionId::CloseTab,
            &state("zsh").expect("fixture"),
            Some(&key())
        )
        .is_none(),
        "an idle shell closes at once"
    );
}

/// A close leaves the tab before the host answers, a second close of it is not
/// recorded, and the host's own removal still fits.
///
/// # Panics
///
/// When the tab stays, the second close is recorded, or a delta is a failure.
#[test]
fn a_close_shows_before_the_host_answers() {
    let mut held = state("zsh").expect("fixture");
    let host = key().host;
    let close = SessionCommand::CloseTab { tab: TabId(2) };
    assert!(
        held.show_submitted(&host, CommandId(7), close.clone(), Instant::now()),
        "the close is showing"
    );
    assert!(
        !held.show_submitted(&host, CommandId(8), close, Instant::now()),
        "a second close of the same tab is not recorded"
    );
    assert_eq!(
        held.model().host(&host).expect("host").pending.len(),
        1,
        "one close is in flight"
    );
    for (generation, delta) in [
        (2_u64, Delta::TabRemoved { tab: TabId(2) }),
        (
            3,
            Delta::SessionRemoved {
                session: SessionId(1),
            },
        ),
    ] {
        held.apply(
            &host,
            &ToClient::Delta {
                generation: Generation(generation),
                payload: encode_delta(&delta).expect("delta encodes"),
            },
        );
    }
    assert!(
        held.take_notices()
            .iter()
            .all(|notice| notice.kind != NoticeKind::Failure),
        "the host's own removal is not a failure"
    );
    assert!(
        held.model()
            .host(&host)
            .expect("host")
            .model
            .sessions
            .is_empty(),
        "the tab and its session are gone"
    );
}

/// A session with more than one tab asks, a running program asks, and either
/// question can be turned off. An idle shell does not ask.
///
/// # Panics
/// Panics when a close asks or skips against those rules.
#[test]
fn closing_asks_for_a_running_program_or_a_session_with_many_tabs() {
    assert!(Settings::default().confirm_close.session);
    assert!(Settings::default().confirm_close.running);
    let idle = session_state(&["zsh", "bash"]).expect("fixture");
    let Some(Step::Confirm(asked)) = begin(ActionId::CloseSession, &idle, Some(&key())) else {
        panic!("a session with two tabs asks");
    };
    assert_eq!(asked.title, "Close \u{201C}work\u{201D}?");
    assert!(asked.detail.contains("2 tabs"), "{}", asked.detail);
    assert!(asked.detail.contains("idle"), "{}", asked.detail);
    assert_eq!(asked.confirm, "Close Session");
    assert!(
        begin(ActionId::CloseTab, &idle, Some(&key())).is_none(),
        "an idle tab closes at once"
    );
    let off = CloseAsks {
        session: false,
        running: false,
    };
    assert!(
        begin_asking(ActionId::CloseSession, &idle, Some(&key()), &[], off).is_none(),
        "the session question can be turned off"
    );
    let running = state("vim").expect("fixture");
    assert!(
        begin_asking(ActionId::CloseTab, &running, Some(&key()), &[], off).is_none(),
        "the running question can be turned off"
    );
    let quiet_session = CloseAsks {
        session: false,
        running: true,
    };
    let Some(Step::Confirm(program)) = begin_asking(
        ActionId::CloseSession,
        &state("vim").expect("fixture"),
        Some(&key()),
        &[],
        quiet_session,
    ) else {
        panic!("one running tab still asks when only the program question is on");
    };
    assert!(
        program.detail.contains("vim is running"),
        "{}",
        program.detail
    );
    assert_eq!(program.confirm, "Close Session");
    let both = session_state(&["vim", "htop"]).expect("fixture");
    programs_and_a_batch_ask_once(&both).expect("named programs and one batch question");
}

/// A session that holds running programs names them, and closing those tabs asks once.
///
/// # Errors
/// Returns the text that did not match when a question is missing or worded wrong.
fn programs_and_a_batch_ask_once(both: &EngineState) -> Result<(), String> {
    let Some(Step::Confirm(batch)) = begin(ActionId::CloseSession, both, Some(&key())) else {
        return Err("tabs and programs are both named".to_owned());
    };
    if !batch.detail.contains("vim and htop are running") || !batch.detail.contains("2 tabs") {
        return Err(batch.detail);
    }
    let tabs = [
        SessionCommand::CloseTab { tab: TabId(2) },
        SessionCommand::CloseTab { tab: TabId(3) },
    ];
    let Some(asked_tabs) = question(both, &key().host, &tabs, CloseAsks::default()) else {
        return Err("closing two tabs, one of them running, asks once".to_owned());
    };
    if asked_tabs.confirm != "Close 2 Tabs" {
        return Err(asked_tabs.confirm);
    }
    if asked_tabs.answer
        != (Answer::Commands {
            host: key().host,
            commands: tabs.to_vec(),
        })
    {
        return Err("the batch sends both closes".to_owned());
    }
    Ok(())
}

/// A state holding one session, one tab per pane title, starting at tab 2.
///
/// # Errors
/// Returns the encoding failure when the fixture model does not encode.
fn session_state(names: &[&str]) -> Result<EngineState, Box<dyn std::error::Error>> {
    let mut next = 2_u64;
    let mut tabs = Vec::new();
    for title in names {
        let id = next;
        next = next.saturating_add(1);
        tabs.push(Tab {
            id: TabId(id),
            name: "shell".to_owned(),
            layout: LayoutNode::Leaf(PaneId(id)),
            panes: vec![Pane {
                id: PaneId(id),
                title: (*title).to_owned(),
                working_directory: None,
                columns: 80,
                rows: 24,
            }],
        });
    }
    let model = HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs,
        }],
    };
    let mut state = EngineState::new();
    state.apply(
        &HostId("build".to_owned()),
        &ToClient::Snapshot {
            generation: model.generation,
            payload: encode_host_model(&model)?,
        },
    );
    Ok(state)
}
