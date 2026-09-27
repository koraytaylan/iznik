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

    /// The server can replace itself in place and keep the sessions it holds.
    ///
    /// A client offers that choice only when the server's `Hello` carries this
    /// bit. It gates nothing a client sends: the adoption record never leaves
    /// the host, so a server without the bit is not missing a feature, and a
    /// client that sees it does nothing different on the wire.
    pub const ADOPT: Capabilities = Capabilities { bits: 1 << 3 };

    /// The server's `Hello` ends with the [`DaemonInstance`] it comes from.
    ///
    /// A client that sets this bit in its own `Hello` can read the field, and
    /// a server appends it only for such a client: an older client refuses a
    /// `Hello` longer than the fields it knows. A server that sets it in its
    /// reply is one whose reply carries the field.
    ///
    /// [`DaemonInstance`]: crate::identity::DaemonInstance
    pub const INSTANCE: Capabilities = Capabilities { bits: 1 << 4 };

    /// The server's `PaneChannel` ends with the sequence before which the
    /// host's own emulator has already answered every terminal query.
    ///
    /// Sent, like [`Capabilities::INSTANCE`], only to a client whose `Hello`
    /// carried the bit, because an older client refuses the longer message.
    /// A client told it must not send the answers its emulator produces from
    /// bytes before that sequence: the program has had them once.
    pub const ANSWERED: Capabilities = Capabilities { bits: 1 << 5 };

    /// The server takes an `Identify` naming the client, and remembers what
    /// it answered that client's recent commands across connections: a
    /// command sent again under the same number is answered from memory,
    /// not applied a second time.
    ///
    /// A client sends `Identify` only to a server whose `Hello` carried this
    /// bit, because an older server refuses a tag it does not know as garbage
    /// and ends the connection on it — which is why the identity is a message
    /// of its own rather than a field appended to the client's `Hello`, which
    /// is sent before the client knows what the server can read.
    pub const IDENTIFY: Capabilities = Capabilities { bits: 1 << 6 };

    /// The server's `Hello` ends, after the [`DaemonInstance`], with the
    /// [`BuildDigest`] of the binary its daemon was started from.
    ///
    /// Appended like [`Capabilities::INSTANCE`], and only after it: a server
    /// sends it to a client whose `Hello` carried both bits, and only when
    /// its daemon knows its own digest — read once, when it started, from the
    /// file the bootstrap wrote beside the binary. A reply that sets the bit
    /// is one that carries the field.
    ///
    /// [`DaemonInstance`]: crate::identity::DaemonInstance
    /// [`BuildDigest`]: crate::identity::BuildDigest
    pub const BUILD: Capabilities = Capabilities { bits: 1 << 7 };

    /// The server writes a file into a pane's directory.
    ///
    /// A client sends `Upload` only to a server whose `Hello` carried this
    /// bit. An older server refuses the tag as garbage and ends the
    /// connection, the same way an unknown command used to, so the bit is
    /// what keeps a paste of a file from taking the link down.
    pub const UPLOAD: Capabilities = Capabilities { bits: 1 << 8 };

    /// The bits that append a field to a server message, and so are claimed
    /// in the server's reply only when the client's `Hello` claimed them too.
    pub const APPENDED: Capabilities = Capabilities {
        bits: Capabilities::INSTANCE.bits | Capabilities::ANSWERED.bits | Capabilities::BUILD.bits,
    };

    /// Every bit this version of the protocol knows.
    const KNOWN: u32 = Capabilities::ZSTD.bits
        | Capabilities::RESUME.bits
        | Capabilities::REORDER_SESSIONS.bits
        | Capabilities::ADOPT.bits
        | Capabilities::INSTANCE.bits
        | Capabilities::ANSWERED.bits
        | Capabilities::IDENTIFY.bits
        | Capabilities::BUILD.bits
        | Capabilities::UPLOAD.bits;

    /// The bits that gate something a person uses, as opposed to something an
    /// optimization is made of.
    ///
    /// A server without `ZSTD` or `RESUME` is a server this client still talks
    /// to with every feature: compression is a saving and resuming is a
    /// recovery, and neither is a command a person chooses. A server without
    /// [`Capabilities::REORDER_SESSIONS`] or [`Capabilities::UPLOAD`] is
    /// missing a feature, and that is what an upgrade offer is made of.
    const FEATURES: u32 = Capabilities::REORDER_SESSIONS.bits | Capabilities::UPLOAD.bits;

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
