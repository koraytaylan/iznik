//! Output delivery receipts whose identity cannot be recycled with a channel number.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::host::identity::HostId;
use iznik_protocol::identity::PaneId;

/// One incarnation of a pane channel. A held receipt keeps this allocation alive,
/// so a later stream can never compare equal merely by reusing its numbers.
#[derive(Debug)]
struct Stream {
    /// Host whose connection owns the channel.
    host: HostId,
    /// Pane whose output is carried.
    pane: PaneId,
    /// Nonzero wire channel announced for this incarnation.
    channel: u8,
}

/// One delivery's count and shared return state, retained by every receipt clone.
#[derive(Debug)]
struct Delivery {
    /// Exact stream which produced these bytes.
    stream: Arc<Stream>,
    /// Exact amount of flow-control credit this delivery can earn.
    bytes: u32,
    /// Atomic once-only flag shared by copies queued from different callers.
    returned: AtomicBool,
}

/// Opaque credit earned by consuming one engine output delivery.
///
/// Cloning does not duplicate credit. Return this receipt through the manager
/// after consuming its bytes; a replaced stream cannot redeem an old receipt.
#[derive(Clone, Debug)]
pub struct CreditReceipt(Arc<Delivery>);

impl PartialEq for CreditReceipt {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CreditReceipt {}

impl CreditReceipt {
    /// Host that delivered the bytes, independent of its current connection.
    #[must_use]
    pub fn host(&self) -> &HostId {
        &self.0.stream.host
    }

    /// Pane that produced the bytes.
    #[must_use]
    pub fn pane(&self) -> PaneId {
        self.0.stream.pane
    }

    /// Exact delivered count, which cannot be changed when returning the receipt.
    #[must_use]
    pub fn bytes(&self) -> u32 {
        self.0.bytes
    }
}

/// A current, previously unreturned receipt admitted at the carrying boundary.
#[derive(Debug)]
pub struct CreditGrant {
    /// Host whose task may carry this grant.
    pub host: HostId,
    /// Pane whose accounting records it.
    pub pane: PaneId,
    /// Wire destination, valid only in the current owning task turn.
    pub channel: u8,
    /// Delivered bytes admitted exactly once.
    pub bytes: u32,
}

/// Current stream identities, protected by the manager's shared credit mutex.
#[derive(Debug, Default)]
pub struct CreditStreams {
    /// Host-qualified panes; channel aliases are invalidated on every announcement.
    streams: BTreeMap<(HostId, PaneId), Arc<Stream>>,
}

impl CreditStreams {
    /// Replace both the pane's prior stream and any prior owner of this host's channel.
    pub fn open(&mut self, host: &HostId, pane: PaneId, channel: u8) {
        self.streams.retain(|(held_host, held_pane), stream| {
            held_host != host || (*held_pane != pane && stream.channel != channel)
        });
        if channel != 0 {
            self.streams.insert(
                (host.clone(), pane),
                Arc::new(Stream {
                    host: host.clone(),
                    pane,
                    channel,
                }),
            );
        }
    }

    /// Forget all streams of a lost connection without affecting another host.
    pub fn disconnect(&mut self, host: &HostId) {
        self.streams.retain(|(held_host, _), _| held_host != host);
    }

    /// Forget a detached pane's stream; held receipts remain expired forever.
    pub fn detach(&mut self, host: &HostId, pane: PaneId) {
        self.streams.remove(&(host.clone(), pane));
    }

    /// Bind a delivered byte count to the current stream, without returning anything yet.
    #[must_use]
    pub fn receipt(&self, host: &HostId, pane: PaneId, bytes: u32) -> Option<CreditReceipt> {
        let stream = Arc::clone(self.streams.get(&(host.clone(), pane))?);
        Some(CreditReceipt(Arc::new(Delivery {
            stream,
            bytes,
            returned: AtomicBool::new(false),
        })))
    }

    /// Admit a receipt once, only while its exact stream incarnation remains current.
    #[must_use]
    pub fn claim(&self, receipt: &CreditReceipt) -> Option<CreditGrant> {
        let stream = self
            .streams
            .get(&(receipt.host().clone(), receipt.pane()))?;
        if !Arc::ptr_eq(stream, &receipt.0.stream)
            || receipt.0.returned.swap(true, Ordering::AcqRel)
        {
            return None;
        }
        Some(CreditGrant {
            host: stream.host.clone(),
            pane: stream.pane,
            channel: stream.channel,
            bytes: receipt.bytes(),
        })
    }
}

impl super::HostManager {
    /// Return credit to the pane's current stream, retaining that identity in the queued order.
    /// Use `credit_receipt` for consumption that may outlive the delivering stream.
    ///
    /// # Errors
    /// Returns an unknown host, missing stream, poisoned credit registry or closed order channel.
    pub fn credit(&self, alias: &str, pane: PaneId, bytes: u32) -> Result<(), super::ManagerError> {
        let host = HostId(alias.to_owned());
        self.shared
            .with(&host, |_| ())
            .ok_or_else(|| super::ManagerError::UnknownHost { host: host.clone() })?;
        let receipt = self
            .shared
            .credit
            .lock()
            .map_err(|_poisoned| super::ManagerError::Poisoned {
                what: "credit registry",
            })?
            .receipt(&host, pane, bytes)
            .ok_or(super::ManagerError::NotCarrying { host, pane })?;
        self.credit_receipt(&receipt)
    }

    /// Queue the exact credit earned by one consumed delivery.
    /// Clones can be submitted safely; the owning host task admits a receipt at most once
    /// and ignores it if its original stream has expired. Submission does not redeem it.
    ///
    /// # Errors
    /// Returns the manager's unknown-host, poisoned-handle or closed-channel error.
    /// A rejected submission leaves the receipt unconsumed and available for retry.
    pub fn credit_receipt(&self, receipt: &CreditReceipt) -> Result<(), super::ManagerError> {
        self.order(
            &receipt.host().0,
            super::Order::Credit {
                receipt: receipt.clone(),
            },
        )
    }
}
