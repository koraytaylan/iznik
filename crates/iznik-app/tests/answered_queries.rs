//! A terminal query the host already answered is not answered again when its
//! bytes are replayed after a resume, and one it did not answer still is.

use iznik_app::bridge::EngineBridge;
use iznik_app::vt::{TerminalTheme, VtOptions, VtThread};
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::Sequence;

mod support;
use support::{key, open, receive, snapshot};

/// A cursor position request, which every emulator answers.
const QUERY: &[u8] = b"\x1b[6n";

/// What a feed of two queries answers when the host already answered the
/// first `answered` bytes.
///
/// # Errors
/// Returns thread or emulator failures.
///
/// # Panics
/// Fails if the thread misses a reply deadline.
fn replies(answered: u64) -> Result<String, Box<dyn std::error::Error>> {
    let thread = VtThread::start(VtOptions::default())?;
    open(&thread, Sequence(0), 80, 24)?;
    let bytes = [QUERY, QUERY].concat();
    EngineBridge::feed_terminal(
        &thread,
        &ManagerEvent::Bytes {
            host: key().host,
            pane: key().pane,
            sequence: Sequence(0),
            bytes,
            receipt: None,
            answered_through: Sequence(answered),
        },
        &TerminalTheme::default(),
    )?;
    if answered > 0 {
        // The position is said on its own, before the feed it applies to.
        let _said = receive(&thread);
    }
    Ok(String::from_utf8(snapshot(&thread)?.responses)?)
}

/// Queries the host answered while nobody was attached are not answered
/// again; the ones after them are.
///
/// # Panics
/// Fails when a replayed query is answered or a live one is not.
#[test]
fn a_replayed_query_is_not_answered_twice() {
    let live = replies(0).expect("both answered");
    assert_eq!(
        live.matches('R').count(),
        2,
        "a live feed answers both: {live:?}"
    );
    let first = u64::try_from(QUERY.len()).expect("a short query");
    let resumed = replies(first).expect("one answered");
    assert_eq!(
        resumed.matches('R').count(),
        1,
        "only the query after what the host answered is answered: {resumed:?}"
    );
    let all = replies(first.saturating_mul(2)).expect("none answered");
    assert!(
        all.is_empty(),
        "nothing the host answered is answered: {all:?}"
    );
}
