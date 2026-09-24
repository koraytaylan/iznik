//! Commands the emulator thread finds queued together, applied as one batch.
//!
//! A flood of output arrives as many chunks. Each is fed to its emulator as
//! it comes, but the snapshot — reading every changed row into owned cells —
//! is taken once per pane per batch, carrying the credit and the delivery
//! receipt of every chunk it covers. Any other command for a pane first
//! publishes that pane's pending snapshot, so what the window sees keeps the
//! order in which commands were sent. Keystrokes typed while a pane waits for
//! a screen are kept and sent once it arrives.

use std::collections::BTreeMap;

use crate::input::TerminalInput;

use iznik_client::host::manager::credit::CreditReceipt;
use iznik_protocol::identity::Sequence;

use super::{PaneKey, PaneTerminal, VtCommand, VtError, VtOptions, VtOutput, apply};

/// Keystrokes and pastes held per pane while it waits for a screen: enough
/// for a burst of typing, bounded so a pane that never recovers cannot grow
/// without end.
const MAXIMUM_QUEUED_INPUT: usize = 256;

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
    /// Keystrokes and pastes typed while a pane had no authoritative screen,
    /// sent in order once its screen arrives.
    queued: BTreeMap<PaneKey, Vec<TerminalInput>>,
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
            queued: BTreeMap::new(),
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
            // Said before every chunk a resume sends again: it changes only
            // what the chunks after it answer, so it neither publishes the
            // pane's pending snapshot nor publishes anything of its own.
            VtCommand::Answered { key, through } => {
                if let Some(pane) = self.panes.get_mut(&key) {
                    pane.answered = pane.answered.max(through);
                }
            }
            VtCommand::Input { key, input } if self.waiting(&key) && typed(&input) => {
                self.flush(&key, results);
                let queue = self.queued.entry(key.clone()).or_default();
                if queue.len() < MAXIMUM_QUEUED_INPUT {
                    queue.push(input);
                }
                results.push((key, Err(VtError::NeedsScreen)));
            }
            other => {
                let key = other.key().clone();
                let screen = matches!(other, VtCommand::Screen { .. });
                let closed = matches!(other, VtCommand::Close(_));
                self.flush(&key, results);
                let result = apply(
                    &mut self.panes,
                    &mut self.pending_size,
                    other,
                    &self.options,
                );
                let replaced = screen && result.is_ok();
                results.push((key.clone(), result));
                if closed {
                    self.queued.remove(&key);
                }
                if replaced {
                    self.send_queued(&key, results);
                }
            }
        }
    }

    /// Whether `key` has no authoritative screen to encode input against.
    fn waiting(&self, key: &PaneKey) -> bool {
        self.panes
            .get(key)
            .is_none_or(|pane| pane.sequence.is_none())
    }

    /// Encode the input typed while `key` waited, now that its screen is here.
    fn send_queued(&mut self, key: &PaneKey, results: &mut Results) {
        for input in self.queued.remove(key).unwrap_or_default() {
            let command = VtCommand::Input {
                key: key.clone(),
                input,
            };
            let result = apply(
                &mut self.panes,
                &mut self.pending_size,
                command,
                &self.options,
            );
            results.push((key.clone(), result));
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
    /// which is where its failure is reported. A chunk with a receipt after
    /// chunks without one, or the other way round, publishes the pending
    /// snapshot first and starts the next.
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
        // A snapshot's credit is either all receipts or all plain bytes: a
        // chunk that differs from the ones pending starts a snapshot of its own.
        if self
            .fed
            .get(&key)
            .is_some_and(|fed| fed.receipts.is_empty() != delivery.is_none())
        {
            self.flush(&key, results);
        }
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

/// Whether `input` is something a person typed that must not be lost to a
/// gap: a keystroke or a paste. Pointer gestures and copies name a frame that
/// the new screen replaces, so they are not kept.
fn typed(input: &TerminalInput) -> bool {
    matches!(
        input,
        TerminalInput::Key(_) | TerminalInput::Paste(_) | TerminalInput::ConfirmedPaste(_)
    )
}
