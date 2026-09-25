//! Closing a tab that is running programs asks first; a tab of idle shells
//! closes at once.

use std::time::Instant;

use iznik_app::actions::ActionId;
use iznik_app::host_ui::{EngineState, NoticeKind};
use iznik_app::prompt::{Answer, Step, answer, begin};
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
    let Some(Step::Ask(prompt)) = begin(
        ActionId::CloseTab,
        &state("vim").expect("fixture"),
        Some(&key()),
    ) else {
        panic!("a running tab asks");
    };
    assert!(prompt.question.contains("vim"), "{}", prompt.question);
    assert_eq!(
        answer(&prompt, "", 0),
        Some(Answer::Command {
            host: key().host,
            command: SessionCommand::CloseTab { tab: TabId(2) },
        })
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
