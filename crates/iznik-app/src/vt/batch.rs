//! Commands the emulator thread finds queued together, applied as one batch.
//!
//! A flood of output arrives as many chunks. Each is fed to its emulator as
//! it comes, but the snapshot — reading every changed row into owned cells —
//! is taken once per pane per batch, carrying the credit and the delivery
//! receipt of every chunk it covers. Any other command for a pane first
//! publishes that pane's pending snapshot, so what the window sees keeps the
//! order in which commands were sent.

use std::collections::BTreeMap;

use iznik_client::host::manager::credit::CreditReceipt;
use iznik_protocol::identity::Sequence;

use super::{PaneKey, PaneTerminal, VtCommand, VtError, VtOptions, VtOutput, apply};

/// Results of one batch, in the order they are published.
pub(super) type Results = Vec<(PaneKey, Result<Option<VtOutput>, VtError>)>;

/// Output fed to one pane in this batch whose snapshot is still to be taken.
#[derive(Default)]
struct Fed {
    /// Stream bytes fed, returned as credit with the snapshot.
    consumed: u32,
    /// Delivery receipts of the chunks fed, in order.
    receipts: Vec<CreditReceipt>,
}

/// Every emulator the thread owns, and the output each was fed this batch.
pub(super) struct Owner {
    /// Emulators by pane.
    panes: BTreeMap<PaneKey, PaneTerminal>,
    /// Sizes asked for before a pane's first screen arrived.
    pending_size: BTreeMap<PaneKey, (u16, u16)>,
    /// Panes fed this batch whose snapshot has not been published yet.
    fed: BTreeMap<PaneKey, Fed>,
    /// Limits shared by every pane.
    options: VtOptions,
}

impl Owner {
    /// An owner with no panes.
    pub(super) fn new(options: VtOptions) -> Self {
        Self {
            panes: BTreeMap::new(),
            pending_size: BTreeMap::new(),
            fed: BTreeMap::new(),
            options,
        }
    }

    /// Most commands one batch takes before its results are published.
    pub(super) fn maximum_batch(&self) -> usize {
        self.options.maximum_batch.max(1)
    }

    /// Apply one command, deferring the snapshot of contiguous output.
    pub(super) fn take(&mut self, command: VtCommand, results: &mut Results) {
        match command {
            VtCommand::Feed {
                key,
                sequence,
                bytes,
                receipt,
            } => self.feed(key, sequence, bytes, receipt, results),
            other => {
                let key = other.key().clone();
                self.flush(&key, results);
                let result = apply(
                    &mut self.panes,
                    &mut self.pending_size,
                    other,
                    &self.options,
                );
                results.push((key, result));
            }
        }
    }

    /// Publish the snapshot of every pane fed during this batch.
    pub(super) fn finish(&mut self, results: &mut Results) {
        let keys: Vec<PaneKey> = self.fed.keys().cloned().collect();
        for key in keys {
            self.flush(&key, results);
        }
    }

    /// Feed one chunk, adding it to the pane's pending snapshot. A chunk that
    /// cannot join it — a gap, a receipt for other bytes, a count past the
    /// credit field — is applied on its own after that snapshot is published,
    /// which is where its failure is reported.
    fn feed(
        &mut self,
        key: PaneKey,
        sequence: Sequence,
        bytes: Vec<u8>,
        receipt: Option<CreditReceipt>,
        results: &mut Results,
    ) {
        let credit = u32::try_from(bytes.len()).ok().filter(|credit| {
            self.fed
                .get(&key)
                .is_none_or(|fed| fed.consumed.checked_add(*credit).is_some())
        });
        let checked = super::delivery_receipt(&key, &bytes, receipt.as_ref()).ok();
        let contiguous = self
            .panes
            .get(&key)
            .is_some_and(|pane| pane.sequence == Some(sequence));
        let (Some(_credit), Some(delivery), true) = (credit, checked, contiguous) else {
            self.flush(&key, results);
            let command = VtCommand::Feed {
                key: key.clone(),
                sequence,
                bytes,
                receipt,
            };
            let result = apply(
                &mut self.panes,
                &mut self.pending_size,
                command,
                &self.options,
            );
            results.push((key, result));
            return;
        };
        let Some(pane) = self.panes.get_mut(&key) else {
            return;
        };
        match pane.feed(sequence, &bytes) {
            Ok(consumed) => {
                let fed = self.fed.entry(key).or_default();
                fed.consumed = fed.consumed.saturating_add(consumed);
                fed.receipts.extend(delivery);
            }
            Err(error) => {
                self.flush(&key, results);
                results.push((key, Err(error)));
            }
        }
    }

    /// Publish one pane's pending snapshot, with all the credit and receipts
    /// of the chunks it covers.
    fn flush(&mut self, key: &PaneKey, results: &mut Results) {
        let Some(fed) = self.fed.remove(key) else {
            return;
        };
        let result = self
            .panes
            .get_mut(key)
            .ok_or(VtError::NeedsScreen)
            .and_then(|pane| pane.snapshot(key.clone(), fed.consumed))
            .map(|mut snapshot| {
                snapshot.receipts = fed.receipts;
                Some(VtOutput::Snapshot(Box::new(snapshot)))
            });
        results.push((key.clone(), result));
    }
}
