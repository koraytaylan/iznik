//! Fixed transition histories exercise the same ledger used by the host task.
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::credit::{CreditReceipt, CreditStreams};
use iznik_protocol::identity::PaneId;

#[path = "fixtures/stream_credit.rs"]
mod fixture;
use std::collections::BTreeMap;

/// Run every history, preserving receipts across changes in the stream registry.
///
/// # Errors
/// Returns an invalid fixture receipt reference or a missing delivery stream.
///
/// # Panics
/// Fails when an grants wire grant differs from the committed expectation.
fn history(case: &fixture::Case) -> Result<(), Box<dyn std::error::Error>> {
    let mut streams = CreditStreams::default();
    let mut receipts = BTreeMap::<usize, CreditReceipt>::new();
    for step in case.steps {
        match *step {
            fixture::Step::Open {
                host,
                pane,
                channel,
            } => streams.open(&HostId(host.to_owned()), PaneId(pane), channel),
            fixture::Step::Deliver {
                host,
                pane,
                bytes,
                receipt,
            } => {
                receipts.insert(
                    receipt,
                    streams
                        .receipt(&HostId(host.to_owned()), PaneId(pane), bytes)
                        .ok_or("missing delivery stream")?,
                );
            }
            fixture::Step::Return { receipt, expected } => {
                let receipt = receipts
                    .get(&receipt)
                    .ok_or("missing fixture receipt")?
                    .clone();
                let actual = streams
                    .claim(&receipt)
                    .map(|grant| (grant.host.0, grant.pane.0, grant.channel, grant.bytes));
                let expected = expected.map(|grant| {
                    (
                        grant.host.to_owned(),
                        grant.pane,
                        grant.channel,
                        grant.bytes,
                    )
                });
                assert_eq!(actual, expected, "{}: {step:?}", case.name);
            }
            fixture::Step::Disconnect { host } => streams.disconnect(&HostId(host.to_owned())),
            fixture::Step::Detach { host, pane } => {
                streams.detach(&HostId(host.to_owned()), PaneId(pane));
            }
            fixture::Step::Restart => streams = CreditStreams::default(),
            fixture::Step::Missing { host, pane } => assert!(
                streams
                    .receipt(&HostId(host.to_owned()), PaneId(pane), 1)
                    .is_none(),
                "{}: no credit for a missing/control stream",
                case.name
            ),
        }
    }
    Ok(())
}

/// Prove every grant in the committed transition fixture.
///
/// # Panics
/// Fails if any history cannot execute or admits a different grant.
#[test]
fn stream_credit_matches_every_committed_history() {
    for case in fixture::CASES {
        let result = history(case);
        assert!(result.is_ok(), "{}: {result:?}", case.name);
    }
}

/// A queued receipt is grants against the stream at drain time, not enqueue time.
///
/// # Panics
/// Fails if an old queued return or a duplicate changes the grants grants after replacement.
#[test]
fn stream_credit_queued_returns_check_the_stream_at_drain() {
    let host = HostId("queued".to_owned());
    let pane = PaneId(1);
    let mut streams = CreditStreams::default();
    streams.open(&host, pane, 7);
    let original = streams.receipt(&host, pane, 3).expect("original delivery");
    let (sender, receiver) = std::sync::mpsc::channel();
    sender.send(original).expect("enqueue before replacement");
    streams.open(&host, pane, 7);
    let current = streams
        .receipt(&host, pane, 5)
        .expect("replacement delivery");
    sender.send(current.clone()).expect("enqueue current");
    sender.send(current).expect("enqueue duplicate");
    drop(sender);
    let grants: Vec<_> = receiver
        .try_iter()
        .filter_map(|receipt| streams.claim(&receipt))
        .map(|grant| (grant.host, grant.pane, grant.channel, grant.bytes))
        .collect();
    assert_eq!(
        grants,
        vec![(host, pane, 7, 5)],
        "only the current delivery is grants once"
    );
}

/// # Panics
///
/// When a turn's credit is not summed per stream, is summed across streams,
/// admits a receipt twice or one from a replaced stream, or loses a byte to a
/// total too large for one message.
#[test]
fn stream_credit_sums_a_turn_per_stream() {
    use iznik_client::host::manager::credit::CreditBatch;
    let host = HostId("batched".to_owned());
    let (first, second) = (PaneId(1), PaneId(2));
    let mut streams = CreditStreams::default();
    streams.open(&host, first, 7);
    streams.open(&host, second, 8);
    let stale = streams.receipt(&host, first, 100).expect("a delivery");
    streams.open(&host, first, 7);
    let mut batch = CreditBatch::default();
    let deliveries = [
        streams.receipt(&host, first, 3).expect("a delivery"),
        streams.receipt(&host, second, 4).expect("a delivery"),
        streams.receipt(&host, first, 5).expect("a delivery"),
    ];
    for receipt in deliveries.iter().chain(deliveries.iter()).chain([&stale]) {
        if let Some(grant) = streams.claim(receipt) {
            batch.add(grant);
        }
    }
    let grants: Vec<_> = batch
        .into_grants()
        .into_iter()
        .map(|grant| (grant.pane, grant.channel, grant.bytes))
        .collect();
    assert_eq!(
        grants,
        vec![(first, 7, 8), (second, 8, 4)],
        "one grant per stream, each receipt once, none from a replaced stream"
    );
    let mut huge = CreditBatch::default();
    for _ in 0..3 {
        let receipt = streams
            .receipt(&host, second, u32::MAX / 2)
            .expect("a delivery");
        huge.add(streams.claim(&receipt).expect("current"));
    }
    let total: u64 = huge
        .into_grants()
        .iter()
        .map(|grant| u64::from(grant.bytes))
        .sum();
    assert_eq!(total, u64::from(u32::MAX / 2) * 3, "and no byte is lost");
}

/// # Panics
///
/// When a stream may have more than the most outstanding delivered to it, or
/// what a claimed grant returns is not taken off what it has outstanding.
#[test]
fn stream_credit_refuses_a_host_past_its_window() {
    use iznik_client::host::manager::credit::{MAXIMUM_UNRETURNED_BYTES, Undeliverable};
    let host = HostId("flooding".to_owned());
    let pane = PaneId(1);
    let mut streams = CreditStreams::default();
    streams.open(&host, pane, 7);
    let most = u32::try_from(MAXIMUM_UNRETURNED_BYTES).expect("the most fits a frame count");
    let whole = streams
        .deliver(&host, pane, most)
        .expect("up to the most is taken");
    assert!(
        matches!(
            streams.deliver(&host, pane, 1),
            Err(Undeliverable::Overrun { .. })
        ),
        "one byte past it is not"
    );
    let grant = streams.claim(&whole).expect("a current receipt");
    streams.returned(&grant);
    assert!(
        streams.deliver(&host, pane, 1).is_ok(),
        "and credit returned makes room again"
    );
    assert_eq!(
        streams
            .deliver(&HostId("elsewhere".to_owned()), pane, 1)
            .err(),
        Some(Undeliverable::NoStream),
        "while a pane with no stream is nobody's"
    );
}

/// # Panics
///
/// When credit named by a replaced stream's token is admitted to its
/// successor, or credit named by the current one is not.
#[test]
fn a_stream_token_returns_credit_only_while_its_stream_is_current() {
    let host = HostId("tokens".to_owned());
    let pane = PaneId(1);
    let mut streams = CreditStreams::default();
    streams.open(&host, pane, 1);
    let first = streams
        .receipt(&host, pane, 10)
        .expect("a stream is open")
        .stream_token();
    assert_ne!(first, 0, "a token is never zero");
    assert!(
        streams.receipt_for_stream(&host, pane, first, 10).is_some(),
        "the current stream's token is honoured"
    );
    streams.open(&host, pane, 1);
    let second = streams
        .receipt(&host, pane, 10)
        .expect("a stream is open")
        .stream_token();
    assert_ne!(first, second, "the same channel again is another stream");
    assert!(
        streams.receipt_for_stream(&host, pane, first, 10).is_none(),
        "a replaced stream's token is ignored"
    );
    assert!(
        streams.receipt_for_stream(&host, pane, 0, 10).is_some(),
        "zero names whichever stream is current"
    );
}
