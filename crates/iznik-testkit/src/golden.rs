//! The one JSONL golden loader every golden test in the workspace uses, and
//! the hex encoding the goldens carry bytes in. A golden is the contract; an
//! error here names the file and the line that failed, so a broken fixture
//! is found without a debugger.

use std::fmt::{self, Display, Formatter, Write};
use std::io;
use std::path::{Path, PathBuf};

/// The radix of a hex digit.
const HEX_RADIX: u32 = 16;

/// The digits one byte takes in hex.
const HEX_DIGITS_PER_BYTE: usize = 2;

/// Why a golden could not be loaded, or its hex could not be read.
#[derive(Debug)]
pub enum GoldenError {
    /// The file could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// A line of the file is not JSON.
    Parse {
        /// The file.
        path: PathBuf,
        /// The line, one-based.
        line: usize,
        /// What the parser said.
        source: serde_json::Error,
    },
    /// A hex string is not hex: an even count of hex digits.
    Hex {
        /// The string, as given.
        text: String,
    },
}

impl Display for GoldenError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            GoldenError::Read { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
            GoldenError::Parse { path, line, source } => {
                write!(formatter, "{}:{line}: {source}", path.display())
            }
            GoldenError::Hex { text } => {
                write!(
                    formatter,
                    "`{text}` is not hex: an even count of hex digits"
                )
            }
        }
    }
}

impl std::error::Error for GoldenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GoldenError::Read { source, .. } => Some(source),
            GoldenError::Parse { source, .. } => Some(source),
            GoldenError::Hex { .. } => None,
        }
    }
}

/// The JSON values of a JSONL file, one per non-empty line, in order.
///
/// # Errors
///
/// [`GoldenError::Read`] when the file cannot be read, and
/// [`GoldenError::Parse`] naming the line that is not JSON.
pub fn lines(path: &Path) -> Result<Vec<serde_json::Value>, GoldenError> {
    let text = std::fs::read_to_string(path).map_err(|source| GoldenError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    text.lines()
        .enumerate()
        .filter(|(_index, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| GoldenError::Parse {
                path: path.to_path_buf(),
                line: index.saturating_add(1),
                source,
            })
        })
        .collect()
}

/// The bytes a hex string encodes, two lowercase or uppercase digits per byte.
///
/// # Errors
///
/// [`GoldenError::Hex`] when the string has an odd length or a character
/// that is not a hex digit.
pub fn bytes(hex: &str) -> Result<Vec<u8>, GoldenError> {
    let invalid = || GoldenError::Hex {
        text: hex.to_owned(),
    };
    if !hex.len().is_multiple_of(HEX_DIGITS_PER_BYTE) {
        return Err(invalid());
    }
    hex.as_bytes()
        .chunks(HEX_DIGITS_PER_BYTE)
        .map(|pair| {
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return Err(invalid());
            }
            let text = std::str::from_utf8(pair).map_err(|_error| invalid())?;
            u8::from_str_radix(text, HEX_RADIX).map_err(|_error| invalid())
        })
        .collect()
}

/// The lowercase hex of some bytes, two digits per byte: the form the
/// goldens hold and a failure prints.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        // Writing to a String cannot fail.
        let _written = write!(text, "{byte:02x}");
        text
    })
}
