//! Standard base64 (RFC 4648, with padding), written once for everything
//! that speaks it: a Windows host's `-EncodedCommand`, the command-line
//! tool's byte fields, and a program's OSC 52 clipboard writes.

/// The standard alphabet; a character's position is its six-bit value.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Bits one character carries.
const CHARACTER_BITS: u32 = 6;
/// Bits in one byte.
const BYTE_BITS: u32 = 8;
/// The six bits one character takes from what is pending.
const CHARACTER_MASK: u32 = 0x3F;
/// Characters in one group, padding included.
const GROUP_CHARACTERS: usize = 4;
/// Bytes one group of characters carries.
const GROUP_BYTES: usize = 3;
/// The character that fills out a final group.
const PADDING: u8 = b'=';

/// `bytes` as base64 text, padded to whole groups of four.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let groups = bytes.len().div_ceil(GROUP_BYTES);
    let mut written = String::with_capacity(groups.saturating_mul(GROUP_CHARACTERS));
    for group in bytes.chunks(GROUP_BYTES) {
        let mut pending: u32 = 0;
        let mut waiting: u32 = 0;
        let mut produced = 0_usize;
        for byte in group {
            pending = pending.checked_shl(BYTE_BITS).unwrap_or_default() | u32::from(*byte);
            waiting = waiting.saturating_add(BYTE_BITS);
            while waiting >= CHARACTER_BITS {
                waiting = waiting.saturating_sub(CHARACTER_BITS);
                written.push(symbol(pending.checked_shr(waiting).unwrap_or_default()));
                produced = produced.saturating_add(1);
            }
        }
        if waiting > 0 {
            written.push(symbol(
                pending
                    .checked_shl(CHARACTER_BITS.saturating_sub(waiting))
                    .unwrap_or_default(),
            ));
            produced = produced.saturating_add(1);
        }
        for _padding in produced..GROUP_CHARACTERS {
            written.push(char::from(PADDING));
        }
    }
    written
}

/// The alphabet's character for the low six bits of `value`.
fn symbol(value: u32) -> char {
    let index = usize::try_from(value & CHARACTER_MASK).unwrap_or_default();
    char::from(ALPHABET.get(index).copied().unwrap_or(PADDING))
}

/// The bytes `encoded` stands for, ignoring ASCII whitespace; `None` when it
/// is not valid base64.
///
/// The payload must be whole groups of four, and padding may only end it.
#[must_use]
pub fn decode(encoded: &str) -> Option<Vec<u8>> {
    let characters: Vec<u8> = encoded.bytes().filter(|byte| !is_space(*byte)).collect();
    if characters.len().checked_rem(GROUP_CHARACTERS) != Some(0) {
        return None;
    }
    let data_length = characters
        .iter()
        .rposition(|character| *character != PADDING)
        .map_or(0, |last| last.saturating_add(1));
    let padding = characters.len().saturating_sub(data_length);
    if padding >= GROUP_CHARACTERS.saturating_sub(1) {
        return None;
    }
    let mut output = Vec::with_capacity(
        data_length
            .saturating_mul(GROUP_BYTES)
            .checked_div(GROUP_CHARACTERS)
            .unwrap_or_default(),
    );
    let mut pending: u32 = 0;
    let mut waiting: u32 = 0;
    for character in characters.get(..data_length)? {
        let value = ALPHABET.iter().position(|symbol| symbol == character)?;
        pending = pending.checked_shl(CHARACTER_BITS)? | u32::try_from(value).ok()?;
        waiting = waiting.saturating_add(CHARACTER_BITS);
        if waiting >= BYTE_BITS {
            waiting = waiting.saturating_sub(BYTE_BITS);
            output.push(u8::try_from(pending.checked_shr(waiting)? & u32::from(u8::MAX)).ok()?);
            pending &= 1_u32.checked_shl(waiting)?.saturating_sub(1);
        }
    }
    Some(output)
}

/// Whether `byte` is ASCII whitespace a base64 payload may contain.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}
