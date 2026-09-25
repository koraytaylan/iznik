//! The adoption record, proven over bytes: a generated state comes back as
//! itself, and a record this build does not know is refused by name.

use iznik_protocol::identity::{DaemonInstance, Generation, PaneId, Sequence, SessionId, TabId};
use iznik_protocol::model::{HostModel, LayoutNode, Pane, Session, Tab};
use iznik_server::adopt::{
    ADOPTED_STATE_VERSION, AdoptError, AdoptedPane, AdoptedState, TermiosState, decode_adopted,
    encode_adopted,
};

/// A state with one pane, including bytes no terminal would print.
fn sample() -> AdoptedState {
    let pane = PaneId(7);
    AdoptedState {
        version: ADOPTED_STATE_VERSION,
        instance: DaemonInstance(0x0123_4567_89ab_cdef_0011_2233_4455_6677),
        model: HostModel {
            generation: Generation(4),
            sessions: vec![Session {
                id: SessionId(1),
                name: "work".to_owned(),
                tabs: vec![Tab {
                    id: TabId(2),
                    name: "shell".to_owned(),
                    panes: vec![Pane {
                        id: pane,
                        title: String::new(),
                        working_directory: None,
                        columns: 80,
                        rows: 24,
                    }],
                    layout: LayoutNode::Leaf(pane),
                }],
            }],
        },
        panes: vec![AdoptedPane {
            pane,
            descriptor: 11,
            process_id: 4242,
            sequence: Sequence(90),
            ring: b"hello\0\x1b[31m\xff".to_vec(),
            termios: TermiosState {
                bytes: vec![1, 2, 3, 4],
            },
        }],
    }
}

/// # Panics
///
/// When the bytes do not decode to the state that produced them.
#[test]
fn adopted_state_round_trips_including_bytes_a_terminal_would_not_print() {
    let state = sample();
    let bytes = encode_adopted(&state).expect("the state encodes");
    let decoded = decode_adopted(&bytes).expect("the bytes decode");
    assert_eq!(decoded, state, "the record comes back as itself");
}

/// # Panics
///
/// When a bad record is accepted or the refusal does not name what is wrong.
#[test]
fn adopted_state_refuses_another_version_a_truncated_field_and_trailing_bytes() {
    let state = sample();
    let mut bytes = encode_adopted(&state).expect("the state encodes");
    let Some(first) = bytes.first_mut() else {
        panic!("the record is empty");
    };
    *first = first.wrapping_add(1);
    let refused = decode_adopted(&bytes).expect_err("another version is refused");
    assert!(
        refused.to_string().contains("version"),
        "the refusal names the version: {refused}"
    );
    assert!(
        matches!(refused, AdoptError::Version { .. }),
        "and nothing is guessed: {refused:?}"
    );

    let encoded = encode_adopted(&state).expect("the state encodes");
    let short = encoded.get(..8).unwrap_or_default();
    let truncated = decode_adopted(short).expect_err("a truncated record is refused");
    let said = truncated.to_string();
    assert!(
        said.contains("truncated") || said.contains("oversized"),
        "the refusal names the short field: {said}"
    );

    let mut trailing = encode_adopted(&state).expect("the state encodes");
    trailing.push(0);
    let extra = decode_adopted(&trailing).expect_err("trailing bytes are refused");
    assert!(
        extra.to_string().contains("trailing"),
        "the refusal names the trailing bytes: {extra}"
    );
}
