//! Queued output is fed before one snapshot, and a snapshot reads only the
//! rows the emulator changed.

mod support;

use std::sync::Arc;

use iznik_app::vt::{VtCommand, VtOptions, VtOutput, VtThread};
use iznik_client::host::manager::credit::{CreditReceipt, CreditStreams};
use iznik_protocol::identity::Sequence;
use support::{key, open, receive, snapshot};

/// Wide enough that no chunk wraps.
const COLUMNS: u16 = 12;
/// One changed row between two unchanged ones, and a fourth for the cursor,
/// whose row the emulator marks dirty whenever the cursor leaves it.
const ROWS: u16 = 4;
/// The credit channel the receipts are issued on.
const CHANNEL: u8 = 1;
/// Chunks sent back to back, as a flood of transport frames arrives.
const CHUNKS: usize = 8;
/// Every chunk's bytes: text that neither wraps nor scrolls.
const CHUNK: &[u8] = b"x";

/// Every chunk's credit and receipt comes back, in order, however many
/// snapshots the thread coalesced the chunks into.
///
/// # Panics
/// Fails when credit or a receipt is lost, repeated or reordered.
#[test]
fn queued_chunks_return_every_receipt_in_order() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), COLUMNS, ROWS).expect("open");
    let mut streams = CreditStreams::default();
    streams.open(&key().host, key().pane, CHANNEL);
    let length = u32::try_from(CHUNK.len()).expect("chunk length");
    let sent: Vec<CreditReceipt> = (0..CHUNKS)
        .map(|_| {
            streams
                .receipt(&key().host, key().pane, length)
                .expect("receipt")
        })
        .collect();
    for (index, receipt) in sent.iter().enumerate() {
        thread
            .send(VtCommand::Feed {
                key: key(),
                sequence: Sequence(u64::try_from(index).expect("index")),
                bytes: CHUNK.to_vec(),
                receipt: Some(receipt.clone()),
            })
            .expect("feed");
    }
    let total = u64::try_from(CHUNKS).expect("total");
    let mut returned = Vec::new();
    let mut consumed = 0_u64;
    loop {
        let frame = snapshot(&thread).expect("snapshot");
        consumed = consumed
            .checked_add(u64::from(frame.consumed_bytes))
            .expect("credit total");
        returned.extend(frame.receipts);
        if frame.sequence == Sequence(total) {
            break;
        }
    }
    assert_eq!(consumed, total, "every chunk's bytes are credited once");
    assert_eq!(returned, sent, "every receipt returns, in order");
}

/// A change to one row reads that row again and shares the others with the
/// previous snapshot.
///
/// # Panics
/// Fails when an unchanged row is read again or a changed one is reused.
#[test]
fn unchanged_rows_are_shared_with_the_previous_snapshot() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), COLUMNS, ROWS).expect("open");
    let first = b"one\r\ntwo\r\nsix\r\n".to_vec();
    let next = u64::try_from(first.len()).expect("length");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(0),
            bytes: first,
            receipt: None,
        })
        .expect("rows");
    let before = snapshot(&thread).expect("snapshot");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(next),
            bytes: b"\x1b7\x1b[2;1HTWO\x1b8".to_vec(),
            receipt: None,
        })
        .expect("middle row");
    let after = snapshot(&thread).expect("snapshot");
    assert_eq!(
        after.dirty_rows.get(..3),
        Some([false, true, false].as_slice()),
        "only the changed text row"
    );
    assert!(
        Arc::ptr_eq(&before.rows[0], &after.rows[0]),
        "first row kept"
    );
    assert!(
        Arc::ptr_eq(&before.rows[2], &after.rows[2]),
        "last row kept"
    );
    let text: String = after.rows[1]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(text.starts_with("TWO"), "middle row read again: {text:?}");
}

/// A command other than output publishes the pending snapshot first, so a
/// reply never overtakes the output sent before it.
///
/// # Panics
/// Fails when the snapshot of earlier output arrives after a later reply.
#[test]
fn pending_output_is_published_before_a_later_command() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), COLUMNS, ROWS).expect("open");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(0),
            bytes: CHUNK.to_vec(),
            receipt: None,
        })
        .expect("feed");
    thread
        .send(VtCommand::Resize {
            key: key(),
            columns: COLUMNS,
            rows: ROWS,
        })
        .expect("resize");
    let first = receive(&thread).result.expect("first reply");
    let Some(VtOutput::Snapshot(fed)) = first else {
        panic!("the output's snapshot comes first");
    };
    assert_eq!(
        fed.consumed_bytes,
        u32::try_from(CHUNK.len()).expect("length"),
        "the first snapshot carries the output's credit"
    );
    let resized = snapshot(&thread).expect("resize snapshot");
    assert_eq!(resized.consumed_bytes, 0, "the resize carries no credit");
}

/// Replayed output, each chunk said after how far the host answered, is
/// batched like live output: the position publishes nothing of its own and
/// does not split the pending snapshot.
///
/// # Panics
/// Fails when the position publishes a result or the replay loses credit.
#[test]
fn replayed_output_shares_snapshots_like_live_output() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), COLUMNS, ROWS).expect("open");
    let total = u64::try_from(CHUNKS).expect("total");
    for index in 0..total {
        thread
            .send(VtCommand::Answered {
                key: key(),
                through: Sequence(total),
            })
            .expect("answered");
        thread
            .send(VtCommand::Feed {
                key: key(),
                sequence: Sequence(index),
                bytes: CHUNK.to_vec(),
                receipt: None,
            })
            .expect("feed");
    }
    let mut consumed = 0_u64;
    loop {
        let frame = snapshot(&thread).expect("every result is a snapshot");
        consumed = consumed
            .checked_add(u64::from(frame.consumed_bytes))
            .expect("credit total");
        if frame.sequence == Sequence(total) {
            break;
        }
    }
    assert_eq!(consumed, total, "every replayed byte is credited once");
}
