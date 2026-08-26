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

    /// Every bit this version of the protocol knows.
    const KNOWN: u32 = Capabilities::ZSTD.bits | Capabilities::RESUME.bits;

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
}
