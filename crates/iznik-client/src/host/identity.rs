//! What a host is called, and how one pane anywhere is named.
//!
//! A host is whatever the person typed — an alias out of their own
//! `~/.ssh/config`, or the `unix:` form this crate owns — because that is what
//! they will recognize in a log at three in the morning. A pane is named
//! `iznik://<host>/<pane>`, and that string is the address in diagnostics, in
//! logs and in the contract a native application is written against, so it
//! must survive being written down and read back exactly.

use core::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use iznik_protocol::identity::PaneId;

use crate::transport::LOCAL_PREFIX;

/// What every global address begins with.
pub const ADDRESS_SCHEME: &str = "iznik://";

/// What separates the host from the pane in one.
const ADDRESS_SEPARATOR: char = '/';

/// The character that begins an escape.
const ESCAPE: char = '%';

/// How many hexadecimal digits follow one.
const ESCAPE_DIGITS: usize = 2;

/// The base an escape's digits are in.
const HEXADECIMAL: u32 = 16;

/// The digits an escape is written with.
const DIGITS: &[u8] = b"0123456789ABCDEF";

/// How many bits one hexadecimal digit carries.
const DIGIT_BITS: u32 = 4;

/// The low bits of a byte one digit covers.
const DIGIT_MASK: u8 = 0x0f;

/// A host, by the name its user gave it.
///
/// The name is the user's, not this program's: an alias from their own SSH
/// configuration, or `unix:<path>`. Nothing here canonicalizes it, because a
/// name a person does not recognize is worse than a long one.
///
/// The empty name is not one of them. The type admits it — the field is
/// public, as the architecture asks — but `iznik:///7` is refused when it is
/// read back, deliberately: whoever wrote that meant to name a host, and
/// answering with a host called nothing would carry the mistake further than
/// refusing it does.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(pub String);

impl Display for HostId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl HostId {
    /// The socket this names, when it names one rather than an SSH host.
    #[must_use]
    pub fn local_socket(&self) -> Option<PathBuf> {
        self.0.strip_prefix(LOCAL_PREFIX).map(PathBuf::from)
    }

    /// Whether it reaches a host over SSH rather than a socket here.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        self.local_socket().is_none()
    }
}

/// One pane, anywhere.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlobalPaneId {
    /// The host holding it.
    pub host: HostId,
    /// Its number there.
    pub pane: PaneId,
}

/// Why a string is not a global pane address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressError {
    /// It does not begin with [`ADDRESS_SCHEME`].
    Scheme {
        /// What was given.
        given: String,
    },
    /// There is no host before the separator, or none at all.
    Host {
        /// What was given.
        given: String,
    },
    /// The pane is not a number.
    Pane {
        /// What stood where a number was wanted.
        given: String,
    },
    /// An escape is not two hexadecimal digits.
    Escape {
        /// What stood where an escape was wanted.
        given: String,
    },
}

impl Display for AddressError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            AddressError::Scheme { given } => {
                write!(formatter, "{given:?} does not begin with {ADDRESS_SCHEME}")
            }
            AddressError::Host { given } => {
                write!(formatter, "{given:?} names no host before its pane")
            }
            AddressError::Pane { given } => write!(formatter, "{given:?} is not a pane number"),
            AddressError::Escape { given } => {
                write!(formatter, "{given:?} is not two hexadecimal digits")
            }
        }
    }
}

impl core::error::Error for AddressError {}

/// Whether a byte may stand for itself in an address.
///
/// The unreserved set of RFC 3986, and nothing else. An alias may hold a
/// slash, a colon, a space or a percent — `unix:/tmp/iznik.sock` holds three
/// of them — and every one of those would either end the host early or be read
/// back as something it was not.
const fn plain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// One string with everything that is not a plain byte escaped.
#[must_use]
pub fn escaped(held: &str) -> String {
    let mut written = String::with_capacity(held.len());
    for byte in held.bytes() {
        if plain(byte) {
            written.push(char::from(byte));
            continue;
        }
        written.push(ESCAPE);
        let high = usize::from(byte >> DIGIT_BITS);
        let low = usize::from(byte & DIGIT_MASK);
        for index in [high, low] {
            if let Some(digit) = DIGITS.get(index) {
                written.push(char::from(*digit));
            }
        }
    }
    written
}

/// One escaped string read back.
///
/// # Errors
///
/// [`AddressError::Escape`] when an escape is not two hexadecimal digits, and
/// when what they stand for is not valid UTF-8 once put together.
pub fn unescaped(held: &str) -> Result<String, AddressError> {
    let complain = || AddressError::Escape {
        given: held.to_owned(),
    };
    let mut bytes = Vec::with_capacity(held.len());
    let mut rest = held;
    while let Some(at) = rest.find(ESCAPE) {
        let (before, after) = rest.split_at_checked(at).ok_or_else(complain)?;
        bytes.extend_from_slice(before.as_bytes());
        let digits = after
            .get(ESCAPE.len_utf8()..ESCAPE.len_utf8().saturating_add(ESCAPE_DIGITS))
            .ok_or_else(complain)?;
        // `from_str_radix` would take a sign, and `%+7` is not an escape.
        if !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
            return Err(complain());
        }
        let byte = u8::from_str_radix(digits, HEXADECIMAL).map_err(|_unreadable| complain())?;
        bytes.push(byte);
        rest = after
            .get(ESCAPE.len_utf8().saturating_add(ESCAPE_DIGITS)..)
            .ok_or_else(complain)?;
    }
    bytes.extend_from_slice(rest.as_bytes());
    String::from_utf8(bytes).map_err(|_invalid| complain())
}

impl Display for GlobalPaneId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{ADDRESS_SCHEME}{}{ADDRESS_SEPARATOR}{}",
            escaped(&self.host.0),
            self.pane.0
        )
    }
}

impl core::str::FromStr for GlobalPaneId {
    type Err = AddressError;

    fn from_str(given: &str) -> Result<GlobalPaneId, AddressError> {
        GlobalPaneId::parse(given)
    }
}

impl GlobalPaneId {
    /// One address read back into what it names.
    ///
    /// # Errors
    ///
    /// [`AddressError`] naming which part of it could not be read.
    pub fn parse(given: &str) -> Result<GlobalPaneId, AddressError> {
        let rest = given
            .strip_prefix(ADDRESS_SCHEME)
            .ok_or_else(|| AddressError::Scheme {
                given: given.to_owned(),
            })?;
        // From the right: the host is escaped, so the only separator left in
        // the string is the one this put there.
        let (host, pane) =
            rest.rsplit_once(ADDRESS_SEPARATOR)
                .ok_or_else(|| AddressError::Host {
                    given: given.to_owned(),
                })?;
        // The host is one segment, and a separator inside it was escaped on
        // the way out. Reading `iznik://a/b/7` as the host `a/b` would give
        // one host two addresses, only one of which this ever writes — and an
        // address a program is written against must name one thing.
        if host.is_empty() || host.contains(ADDRESS_SEPARATOR) {
            return Err(AddressError::Host {
                given: given.to_owned(),
            });
        }
        let number = pane.parse().map_err(|_unreadable| AddressError::Pane {
            given: pane.to_owned(),
        })?;
        Ok(GlobalPaneId {
            host: HostId(unescaped(host)?),
            pane: PaneId(number),
        })
    }
}
