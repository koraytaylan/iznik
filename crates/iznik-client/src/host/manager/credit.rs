//! Output delivery receipts whose identity cannot be recycled with a channel number.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
    /// A number no other incarnation in this process has had, which is how a
    /// caller across a boundary that cannot hold a receipt names this one.
    token: u64,
}

/// The next stream token to hand out. Never zero, which says "whichever
/// stream is current".
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

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

    /// The token of the stream that delivered the bytes: never zero, and
    /// never another stream's in this process.
    ///
    /// For a caller that cannot hold the receipt itself — one across the C
    /// boundary — and returns credit by naming the stream instead, with
    /// [`super::HostManager::credit_stream`].
    #[must_use]
    pub fn stream_token(&self) -> u64 {
        self.0.stream.token
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

/// The most bytes a host may have delivered on one pane's stream that this
/// client has not yet returned credit for.
///
/// A host that keeps to flow control never has more outstanding than the
/// largest window it gives a pane — a mebibyte, for the one a person is
/// looking at — because it sends nothing once a window is spent. Four times
/// that is a margin for a server with other figures, not an allowance: a host
/// past it is not being flow-controlled, and every byte it sends is one this
/// client holds for an application that has not asked for more.
pub const MAXIMUM_UNRETURNED_BYTES: u64 = 4 * 1024 * 1024;

/// Why bytes that arrived for a pane cannot be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undeliverable {
    /// No stream is open for the pane; the bytes are nobody's.
    NoStream,
    /// The host sent past its window: this much would be outstanding.
    Overrun {
        /// What would be unreturned with these bytes counted.
        unreturned: u64,
    },
}

/// Current stream identities, protected by the manager's shared credit mutex.
#[derive(Debug, Default)]
pub struct CreditStreams {
    /// Host-qualified panes; channel aliases are invalidated on every announcement.
    streams: BTreeMap<(HostId, PaneId), Arc<Stream>>,
    /// Bytes each current stream has delivered that no claimed receipt has
    /// returned yet.
    unreturned: BTreeMap<(HostId, PaneId), u64>,
}

impl CreditStreams {
    /// Replace both the pane's prior stream and any prior owner of this host's channel.
    pub fn open(&mut self, host: &HostId, pane: PaneId, channel: u8) {
        self.streams.retain(|(held_host, held_pane), stream| {
            held_host != host || (*held_pane != pane && stream.channel != channel)
        });
        let streams = &self.streams;
        self.unreturned
            .retain(|named, _count| streams.contains_key(named));
        if channel != 0 {
            self.streams.insert(
                (host.clone(), pane),
                Arc::new(Stream {
                    host: host.clone(),
                    pane,
                    channel,
                    token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
                }),
            );
        }
    }

    /// Forget all streams of a lost connection without affecting another host.
    pub fn disconnect(&mut self, host: &HostId) {
        self.streams.retain(|(held_host, _), _| held_host != host);
        self.unreturned
            .retain(|(held_host, _), _| held_host != host);
    }

    /// Forget a detached pane's stream; held receipts remain expired forever.
    pub fn detach(&mut self, host: &HostId, pane: PaneId) {
        let named = (host.clone(), pane);
        self.streams.remove(&named);
        self.unreturned.remove(&named);
    }

    /// Takes what an admitted grant returns off what its stream has
    /// outstanding.
    pub fn returned(&mut self, grant: &CreditGrant) {
        if let Some(counted) = self.unreturned.get_mut(&(grant.host.clone(), grant.pane)) {
            *counted = counted.saturating_sub(u64::from(grant.bytes));
        }
    }

    /// Counts bytes that arrived on a pane's stream against what the host may
    /// have outstanding, and binds a receipt for them to the stream.
    ///
    /// # Errors
    ///
    /// [`Undeliverable::NoStream`] when no stream is open for the pane, and
    /// [`Undeliverable::Overrun`] when these bytes would put more than
    /// [`MAXIMUM_UNRETURNED_BYTES`] outstanding on it — a host sending past
    /// its window, which is a protocol error and not a delivery.
    pub fn deliver(
        &mut self,
        host: &HostId,
        pane: PaneId,
        bytes: u32,
    ) -> Result<CreditReceipt, Undeliverable> {
        let receipt = self
            .receipt(host, pane, bytes)
            .ok_or(Undeliverable::NoStream)?;
        let counted = self.unreturned.entry((host.clone(), pane)).or_default();
        let unreturned = counted.saturating_add(u64::from(bytes));
        if unreturned > MAXIMUM_UNRETURNED_BYTES {
            return Err(Undeliverable::Overrun { unreturned });
        }
        *counted = unreturned;
        Ok(receipt)
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

    /// A receipt for bytes of the stream `token` names, only while that stream
    /// is the pane's current one; zero names whichever stream is current.
    ///
    /// A token of a stream that has since been replaced gives nothing: those
    /// bytes were delivered on a stream the host has already forgotten, and
    /// credit for them would be credit on the new one it never earned.
    #[must_use]
    pub fn receipt_for_stream(
        &self,
        host: &HostId,
        pane: PaneId,
        token: u64,
        bytes: u32,
    ) -> Option<CreditReceipt> {
        let stream = self.streams.get(&(host.clone(), pane))?;
        if token != 0 && stream.token != token {
            return None;
        }
        self.receipt(host, pane, bytes)
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

/// Credit claimed in one turn of a host's loop, summed per stream, so that a
/// window returned a delivery at a time goes back as one message per pane.
///
/// Each receipt is still claimed on its own and at most once — the summing is
/// of grants that were already admitted, and only of grants for the same
/// stream: the same pane on the same channel, in a turn in which no stream can
/// have been replaced, because streams are replaced only by what the host
/// says and nothing is heard in the middle of a turn.
#[derive(Debug, Default)]
pub struct CreditBatch {
    /// One grant per stream, in the order each stream was first claimed for.
    grants: Vec<CreditGrant>,
}

impl CreditBatch {
    /// Adds one admitted grant, to its stream's total.
    ///
    /// A total that would pass what one message can say starts another grant
    /// for the same stream rather than saturating, so no byte of credit is
    /// ever lost to the sum.
    pub fn add(&mut self, grant: CreditGrant) {
        let same = self.grants.iter_mut().rev().find(|held| {
            held.host == grant.host && held.pane == grant.pane && held.channel == grant.channel
        });
        if let Some(held) = same
            && let Some(total) = held.bytes.checked_add(grant.bytes)
        {
            held.bytes = total;
            return;
        }
        self.grants.push(grant);
    }

    /// Whether nothing was claimed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// The grants to send, one per stream unless a total needed more.
    #[must_use]
    pub fn into_grants(self) -> Vec<CreditGrant> {
        self.grants
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

    /// Return credit for bytes of the stream `token` names — what a delivery
    /// carried as [`CreditReceipt::stream_token`] — and ignore it when that
    /// stream has since been replaced. Zero names whichever stream is current.
    ///
    /// For a caller that cannot hold the receipt itself. A stale token is not
    /// an error: its bytes were delivered on a stream the host has forgotten,
    /// and the credit simply has nothing left to go to.
    ///
    /// # Errors
    /// Returns an unknown host, poisoned credit registry or closed order channel.
    pub fn credit_stream(
        &self,
        alias: &str,
        pane: PaneId,
        token: u64,
        bytes: u32,
    ) -> Result<(), super::ManagerError> {
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
            .receipt_for_stream(&host, pane, token, bytes);
        receipt.map_or(Ok(()), |receipt| self.credit_receipt(&receipt))
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
