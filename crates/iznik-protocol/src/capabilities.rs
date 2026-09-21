//! The capability bit set exchanged in `Hello`. Unknown bits are preserved,
//! never dropped, so a newer peer round-trips its own advertisement intact
//! through an older one.

/// A set of capability bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Capabilities {
    /// The bits, known and unknown alike.
    bits: u32,
}

impl Capabilities {
    /// Streaming zstd over the whole connection.
    pub const ZSTD: Capabilities = Capabilities { bits: 1 };

    /// Resuming a pane's output from a byte position the client holds.
    pub const RESUME: Capabilities = Capabilities { bits: 1 << 1 };

    /// Reordering the host's whole session list with `ReorderSessions`.
    ///
    /// A server built before that command existed does not set this bit, and a
    /// client must not send the command to it: the tag is one no decoder on
    /// that server claims, so the frame would be refused as garbage and take
    /// the whole connection down. What a server can decode is a capability
    /// rather than something implied by a version, because the two ends are
    /// upgraded separately and a remote is only ever replaced on purpose.
    pub const REORDER_SESSIONS: Capabilities = Capabilities { bits: 1 << 2 };

    /// Every bit this version of the protocol knows.
    const KNOWN: u32 =
        Capabilities::ZSTD.bits | Capabilities::RESUME.bits | Capabilities::REORDER_SESSIONS.bits;

    /// The bits that gate something a person uses, as opposed to something an
    /// optimization is made of.
    ///
    /// A server without `ZSTD` or `RESUME` is a server this client still talks
    /// to with every feature: compression is a saving and resuming is a
    /// recovery, and neither is a command a person chooses. A server without
    /// [`Capabilities::REORDER_SESSIONS`] is missing a feature, and that — and
    /// only that — is what an upgrade offer is made of.
    const FEATURES: u32 = Capabilities::REORDER_SESSIONS.bits;

    /// The set with exactly these bits, whatever they mean.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Capabilities {
        Capabilities { bits }
    }

    /// The bits, exactly as advertised.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.bits
    }

    /// The bits this version of the protocol does not know.
    #[must_use]
    pub const fn unknown_bits(self) -> u32 {
        self.bits & !Capabilities::KNOWN
    }

    /// Every bit this version of the protocol knows, as a set.
    ///
    /// What a server of this build advertises, and so what a connection is
    /// checked against: a server missing one of these is one a client can
    /// still talk to, but not for whatever the missing bit gates.
    #[must_use]
    pub const fn known() -> Capabilities {
        Capabilities {
            bits: Capabilities::KNOWN,
        }
    }

    /// The bits this set is missing out of `wanted`: what `wanted` offers that
    /// this does not.
    ///
    /// The capability gap between two peers, read from the side that knows
    /// more. Empty when this set carries every bit of `wanted`.
    #[must_use]
    pub const fn missing(self, wanted: Capabilities) -> Capabilities {
        Capabilities {
            bits: wanted.bits & !self.bits,
        }
    }

    /// What this set is missing of the bits that gate a person's features, as
    /// opposed to the ones an optimization rests on.
    ///
    /// What an upgrade offer is made of: a server missing one of these cannot
    /// do something this build offers, where one missing compression or resume
    /// simply does it less well.
    #[must_use]
    pub const fn missing_features(self) -> Capabilities {
        Capabilities {
            bits: Capabilities::FEATURES & !self.bits,
        }
    }

    /// Whether this set carries every bit of `wanted`.
    #[must_use]
    pub const fn contains(self, wanted: Capabilities) -> bool {
        self.bits & wanted.bits == wanted.bits
    }
}
