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
