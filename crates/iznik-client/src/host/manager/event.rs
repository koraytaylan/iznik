//! What the manager says happened, as the application hears it.

use iznik_protocol::identity::{Generation, PaneId, Sequence};

use crate::host::identity::HostId;
use crate::host::manager::credit;
use crate::host::state::HostState;
use crate::reduce::Notification;

/// Something the manager says happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagerEvent {
    /// A host moved from one state to another.
    Moved {
        /// The host.
        host: HostId,
        /// Where it is now.
        state: HostState,
    },
    /// A host sent a screen for a pane; whatever is drawn is replaced by it.
    Screen {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
        /// The byte the screen is exact at.
        sequence: Sequence,
        /// Its width in cells.
        columns: u16,
        /// Its height in cells.
        rows: u16,
        /// The bytes that reproduce it, which is what a caller redraws from.
        bytes: Vec<u8>,
    },
    /// The host's whole model, as it said it.
    ///
    /// Carried in the protocol's own encoding rather than as a value of this
    /// crate's, because what is above the manager is a C boundary and one
    /// schema is better than two. `model()` holds the same thing, decoded.
    Snapshot {
        /// The host.
        host: HostId,
        /// The generation it stands at.
        generation: Generation,
        /// The model, as `decode_host_model` reads it.
        payload: Vec<u8>,
    },
    /// One numbered change to it, in the same encoding.
    Delta {
        /// The host.
        host: HostId,
        /// The generation this change produces.
        generation: Generation,
        /// The change, as `decode_delta` reads it.
        payload: Vec<u8>,
    },
    /// A pane's output has stopped arriving on the channel it was on.
    Detached {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
    },
    /// A subscribed pane's output, as it arrives.
    ///
    /// The sequence is the byte the first of them is, so a caller that cares
    /// can see for itself that a stream carried on across a reconnection
    /// rather than starting again.
    Bytes {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
        /// The byte the first of these is.
        sequence: Sequence,
        /// Every terminal query in the pane's bytes before this sequence was
        /// already answered by the host's own emulator.
        ///
        /// An emulator fed these bytes must not send the answers it produces
        /// from the ones before it — the first `answered_through − sequence`
        /// of them, when that is positive — because the program has had them
        /// once. It is only ever ahead of `sequence` for bytes a resume sends
        /// again; a live stream starts at or past it.
        answered_through: Sequence,
        /// What arrived.
        bytes: Vec<u8>,
        /// Delivery-bound credit for these bytes. Engine deliveries always carry it;
        /// synthetic/offline events may omit it and have only current-stream credit semantics.
        receipt: Option<credit::CreditReceipt>,
    },
    /// A host began carrying a pane's output, from `sequence` on: the answer
    /// to a subscribe or a resume, whether or not a screen comes with it.
    Carried {
        /// The host.
        host: HostId,
        /// The pane.
        pane: PaneId,
        /// The first byte the host will send.
        sequence: Sequence,
    },
    /// Something worth telling whoever is watching.
    ///
    /// Among them [`Notification::DaemonRestarted`], when a connection finds
    /// the host's daemon is another run than the last one reached: whatever
    /// draws a pane should start it again then rather than keep what it holds,
    /// because pane numbers begin again with every daemon and a resume from a
    /// byte of the old run is asked for afresh.
    Notify(Notification),
    /// A host is no longer held at all.
    Removed {
        /// The host.
        host: HostId,
    },
}

/// How many of the `length` bytes starting at `sequence` come before
/// `answered_through`: the leading bytes of a delivery whose terminal queries
/// the host already answered, and whose answers an emulator must not send.
#[must_use]
pub fn answered_length(sequence: Sequence, answered_through: Sequence, length: usize) -> usize {
    let before = answered_through.0.saturating_sub(sequence.0);
    usize::try_from(before).map_or(length, |before| before.min(length))
}
