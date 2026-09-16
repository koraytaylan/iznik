//! Delivery identities survive the bridge, native owner and accepted grid frame.

#[path = "support/engine.rs"]
mod engine;
mod support;

use gpui_kit::{TestAppContext, WindowHandle};
use iznik_app::bridge::EngineBridge;
use iznik_app::grid::{GridMetrics, TerminalGrid};
use iznik_app::vt::{TerminalSnapshot, TerminalTheme, VtCommand, VtError, VtOptions, VtThread};
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::manager::credit::{CreditReceipt, CreditStreams};
use iznik_protocol::identity::Sequence;
use support::{key, open, receive, snapshot};

/// Width sufficient for each short delivery without wrapping.
const COLUMNS: u16 = 12;
/// Two rows expose accidental output without retaining a large fixture.
const ROWS: u16 = 2;
/// An initial channel whose replacement has a different incarnation.
const CHANNEL: u8 = 7;
/// Fixed delivery amount used in the native and grid paths.
const OUTPUT: &[u8] = b"abc";
/// Fixture failures are reported by the headless test wrapper.
type Failed = Box<dyn std::error::Error>;

/// Report fixture errors outside the generated GPUI test wrapper.
///
/// # Panics
/// Fails with the exact fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Feed one receipt through the production event bridge and native owner.
///
/// # Errors
/// Returns thread, emulator or snapshot failures.
///
/// # Panics
/// Fails if the bounded native reply never arrives.
fn deliver(thread: &VtThread, receipt: &CreditReceipt) -> Result<TerminalSnapshot, Failed> {
    EngineBridge::feed_terminal(
        thread,
        &ManagerEvent::Bytes {
            host: key().host,
            pane: key().pane,
            sequence: Sequence(0),
            bytes: OUTPUT.to_vec(),
            receipt: Some(receipt.clone()),
        },
        &TerminalTheme::default(),
    )?;
    snapshot(thread)
}

/// Create a current delivery in the production ledger without a transport.
///
/// # Errors
/// Returns a missing stream or an invalid fixture byte count.
fn receipt(streams: &CreditStreams) -> Result<CreditReceipt, Failed> {
    streams
        .receipt(&key().host, key().pane, u32::try_from(OUTPUT.len())?)
        .ok_or_else(|| "missing fixture stream".into())
}

#[gpui_kit::test]
fn stream_credit_survives_bridge_native_grid_and_retry(context: &mut TestAppContext) {
    check(&retry(context));
}

/// A rejected engine submission and a duplicate frame cannot lose or multiply a receipt.
///
/// # Errors
/// Returns engine, native, grid or window fixture failures.
///
/// # Panics
/// Fails if identity, exact bytes or retry behavior changes at any boundary.
fn retry(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let grid = context.add_window(|_, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let initial = open(&thread, Sequence(0), COLUMNS, ROWS)?;
    grid.update(context, |grid, _, context| grid.apply(initial, context))??;
    let mut streams = CreditStreams::default();
    streams.open(&key().host, key().pane, CHANNEL);
    let receipt = receipt(&streams)?;
    let frame = deliver(&thread, &receipt)?;
    assert_eq!(
        frame.receipt.as_ref(),
        Some(&receipt),
        "bridge and native owner retain identity"
    );
    assert_eq!(
        frame.consumed_bytes,
        receipt.bytes(),
        "native consumption matches delivery"
    );
    grid.update(context, |grid, _, context| {
        grid.apply(frame.clone(), context)
    })??;
    grid.update(context, |grid, _, context| grid.apply(frame, context))??;
    let (bridge, _directory) = engine::start("receipt-retry")?;
    grid.update(context, |grid, _, _| {
        assert!(
            bridge.flush_terminal_credit(grid).is_err(),
            "unattached engine rejects the pending receipt"
        );
        assert!(
            grid.flush_receipts(|_| Err("rejected")).is_err(),
            "failed engine submission retained the receipt"
        );
        let mut returned = Vec::new();
        grid.flush_receipts::<Failed>(|held| {
            assert_eq!(held, &receipt, "retry preserves the original identity");
            returned.push(streams.claim(held).ok_or("receipt lost")?.bytes);
            Ok(())
        })?;
        assert_eq!(
            returned,
            vec![receipt.bytes()],
            "duplicate frames earn credit once"
        );
        grid.flush_receipts::<Failed>(|_| Err("duplicate return".into()))?;
        grid.flush_credit::<Failed>(|_, _| Err("receipt became unbound credit".into()))?;
        Ok::<(), Failed>(())
    })??;
    Ok(())
}

#[gpui_kit::test]
fn stream_credit_reset_preserves_delivery_identity(context: &mut TestAppContext) {
    check(&replacement(context));
}

/// A screen reset preserves pending original receipts but cannot retarget them to the new stream.
///
/// # Errors
/// Returns native, ledger, grid or window fixture failures.
///
/// # Panics
/// Fails if the original frame or screen earns replacement credit.
fn replacement(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let grid = context.add_window(|_, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let initial = open(&thread, Sequence(0), COLUMNS, ROWS)?;
    grid.update(context, |grid, _, context| grid.apply(initial, context))??;
    let mut streams = CreditStreams::default();
    streams.open(&key().host, key().pane, CHANNEL);
    let original = receipt(&streams)?;
    let frame = deliver(&thread, &original)?;
    grid.update(context, |grid, _, context| grid.apply(frame, context))??;
    streams.open(&key().host, key().pane, CHANNEL);
    let reset = open(&thread, Sequence(0), COLUMNS, ROWS)?;
    assert!(
        reset.receipt.is_none(),
        "screen replay has no delivery receipt"
    );
    assert_eq!(
        reset.consumed_bytes, 0,
        "screen replay consumes no stream bytes"
    );
    grid.update(context, |grid, _, context| grid.apply(reset, context))??;
    let current = receipt(&streams)?;
    let replacement = deliver(&thread, &current)?;
    grid.update(context, |grid, _, context| grid.apply(replacement, context))??;
    assert_returns(context, grid, &streams, &original, &current)
}

/// Inspect the retained queue after replacement without substituting count-based credit.
///
/// # Errors
/// Returns a closed-window or queue failure.
///
/// # Panics
/// Fails if either receipt was replaced, dropped or returned out of order.
fn assert_returns(
    context: &mut TestAppContext,
    grid: WindowHandle<TerminalGrid>,
    streams: &CreditStreams,
    original: &CreditReceipt,
    current: &CreditReceipt,
) -> Result<(), Failed> {
    grid.update(context, |grid, _, _| {
        let mut seen = Vec::new();
        let mut grants = Vec::new();
        grid.flush_receipts::<Failed>(|held| {
            seen.push(held.clone());
            if let Some(grant) = streams.claim(held) {
                grants.push(grant.bytes);
            }
            Ok(())
        })?;
        assert_eq!(
            seen,
            vec![original.clone(), current.clone()],
            "reset retains both pending identities in order"
        );
        assert_eq!(
            grants,
            vec![current.bytes()],
            "only the replacement delivery earns credit"
        );
        grid.flush_credit::<Failed>(|_, _| Err("unexpected unbound credit".into()))
    })??;
    Ok(())
}

/// Invalid receipt metadata is rejected before any native bytes are consumed.
///
/// # Panics
/// Fails if invalid output changes the sequence or if subsequent valid output fails.
#[test]
fn stream_credit_native_rejects_an_invalid_delivery() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), COLUMNS, ROWS).expect("screen");
    let mut streams = CreditStreams::default();
    streams.open(&key().host, key().pane, CHANNEL);
    let receipt = streams
        .receipt(&key().host, key().pane, 1)
        .expect("receipt");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(0),
            bytes: OUTPUT.to_vec(),
            receipt: Some(receipt),
        })
        .expect("send");
    assert!(matches!(receive(&thread).result, Err(VtError::Credit)));
    let current = self::receipt(&streams).expect("current receipt");
    assert_eq!(
        deliver(&thread, &current).expect("valid delivery").sequence,
        Sequence(3)
    );
}
