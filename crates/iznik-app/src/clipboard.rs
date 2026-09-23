//! Program clipboard writes carried in terminal output.
//!
//! A full-screen application such as `OpenCode` copies with OSC 52. The text has
//! to reach the clipboard of the machine running this application. libghostty-vt
//! reports a cleared clipboard through a null contents pointer, and reading that
//! pointer aborts, so the bytes are decoded here instead of through that callback.
//! A reconstructed screen is not scanned, so attaching again does not repeat a copy.

use std::mem::take;

/// ESC, the 7-bit introducer of an operating-system command.
const ESCAPE: u8 = 0x1b;
/// The second byte of a 7-bit OSC introducer, after [`ESCAPE`].
const OSC_MARKER: u8 = b']';
/// The second bytes of the 7-bit DCS, SOS, PM and APC introducers: strings
/// whose payload is not terminal output and may carry an escaped OSC — a tmux
/// passthrough does — that the emulator never runs.
const STRING_MARKERS: &[u8] = b"PX^_";
/// BEL, one of the two OSC terminators.
const BELL: u8 = 0x07;
/// The final byte of the string terminator ESC `\`.
const STRING_FINAL: u8 = b'\\';
/// CAN, which cancels an OSC in progress.
const CANCEL: u8 = 0x18;
/// SUB, which cancels an OSC in progress.
const SUBSTITUTE: u8 = 0x1a;
/// Encoded OSC body retained while a clipboard write is assembled.
///
/// A larger write is discarded. One mebibyte of base64 is more than a selection
/// from a full-screen application needs, and the buffer must stay bounded.
const MAXIMUM_BODY_BYTES: usize = 1_048_576;
/// Base64 letters before the lowercase group.
const LETTERS: u8 = 26;
/// Base64 digits after the two letter groups.
const DIGITS: u8 = 10;
/// Sextets in one base64 group.
const GROUP_LENGTH: usize = 4;
/// Index of the third sextet. `0` and `1` are the only bare match values allowed.
const THIRD: usize = 2;
/// Index of the fourth sextet.
const LAST: usize = 3;
/// Decoded bytes in a base64 group with no padding.
const DECODED_BYTES: usize = 3;
/// How far the first sextet moves into the high bits of the first decoded byte.
const FIRST_SHIFT: u32 = 2;
/// How far the second sextet is split between the first and second decoded bytes.
const SECOND_SHIFT: u32 = 4;
/// How far the third sextet moves into the high bits of the third decoded byte.
const THIRD_SHIFT: u32 = 6;
/// The low eight bits of a combined sextet word.
const BYTE_MASK: u32 = 0xff;
/// The most padding characters one base64 group can end with.
const MAXIMUM_PADDING: usize = 2;

/// Where an OSC 52 scan is between output batches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ScanState {
    /// Ordinary terminal text.
    #[default]
    Ground,
    /// Saw ESC and may be entering an OSC.
    Escape,
    /// Inside an OSC body.
    Body,
    /// Saw ESC inside an OSC body and may be ending it.
    BodyEscape,
    /// The body exceeded [`MAXIMUM_BODY_BYTES`]; wait for its terminator.
    Discard,
    /// Saw ESC while discarding an oversized OSC.
    DiscardEscape,
    /// Inside a DCS, SOS, PM or APC payload, which ends only at ESC `\`.
    Skip,
    /// Saw ESC inside such a payload.
    SkipEscape,
}

/// Assembles OSC 52 clipboard writes that may be split across output batches.
#[derive(Clone, Debug, Default)]
pub(crate) struct ClipboardScan {
    /// Parser position.
    state: ScanState,
    /// OSC body collected so far, without the introducer.
    body: Vec<u8>,
}

impl ClipboardScan {
    /// A scan at the start of a pane's output.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record every completed plain-text OSC 52 write in `bytes`.
    ///
    /// An empty payload records an empty string, which is a request to clear
    /// the clipboard. A write that is not valid base64 or not text is ignored.
    pub(crate) fn observe(&mut self, bytes: &[u8], copies: &mut Vec<String>) {
        for byte in bytes {
            self.byte(*byte, copies);
        }
    }

    /// Advance one byte.
    fn byte(&mut self, byte: u8, copies: &mut Vec<String>) {
        match self.state {
            ScanState::Ground => self.ground(byte),
            ScanState::Escape => self.escape(byte),
            ScanState::Body => self.body(byte, copies),
            ScanState::BodyEscape => self.body_escape(byte, copies),
            ScanState::Discard => self.discard(byte),
            ScanState::DiscardEscape => self.discard_escape(byte, copies),
            ScanState::Skip => self.skip(byte),
            ScanState::SkipEscape => self.skip_escape(byte),
        }
    }

    /// Ordinary text, or the start of an escape.
    fn ground(&mut self, byte: u8) {
        self.state = match byte {
            ESCAPE => ScanState::Escape,
            _ => ScanState::Ground,
        };
    }

    /// The byte after ESC.
    fn escape(&mut self, byte: u8) {
        self.state = if byte == OSC_MARKER {
            ScanState::Body
        } else if STRING_MARKERS.contains(&byte) {
            ScanState::Skip
        } else if byte == ESCAPE {
            ScanState::Escape
        } else {
            ScanState::Ground
        };
    }

    /// One body byte, or a terminator.
    fn body(&mut self, byte: u8, copies: &mut Vec<String>) {
        match byte {
            BELL => self.finish(copies),
            ESCAPE => self.state = ScanState::BodyEscape,
            CANCEL | SUBSTITUTE => self.cancel(),
            _ => self.push(byte),
        }
    }

    /// The byte after ESC inside a body. `\` completes the OSC.
    fn body_escape(&mut self, byte: u8, copies: &mut Vec<String>) {
        if byte == STRING_FINAL {
            self.finish(copies);
            return;
        }
        self.cancel();
        self.byte(byte, copies);
    }

    /// Ignore an oversized body until it ends.
    fn discard(&mut self, byte: u8) {
        self.state = match byte {
            BELL => ScanState::Ground,
            ESCAPE => ScanState::DiscardEscape,
            _ => ScanState::Discard,
        };
    }

    /// The byte after ESC while an oversized body is being ignored.
    fn discard_escape(&mut self, byte: u8, copies: &mut Vec<String>) {
        if byte == STRING_FINAL {
            self.state = ScanState::Ground;
            return;
        }
        self.cancel();
        self.byte(byte, copies);
    }

    /// A string payload byte. CAN and SUB abandon the string.
    fn skip(&mut self, byte: u8) {
        self.state = match byte {
            ESCAPE => ScanState::SkipEscape,
            CANCEL | SUBSTITUTE => ScanState::Ground,
            _ => ScanState::Skip,
        };
    }

    /// The byte after ESC in a string payload: `\` ends it, and anything
    /// else — a doubled ESC, an escaped OSC — is still payload.
    fn skip_escape(&mut self, byte: u8) {
        self.state = match byte {
            STRING_FINAL => ScanState::Ground,
            ESCAPE => ScanState::SkipEscape,
            _ => ScanState::Skip,
        };
    }

    /// Keep one body byte, or start discarding once the body reaches its cap.
    fn push(&mut self, byte: u8) {
        if self.body.len() == MAXIMUM_BODY_BYTES {
            self.body.clear();
            self.state = ScanState::Discard;
            return;
        }
        self.body.push(byte);
    }

    /// Decode a completed OSC when it is a clipboard write.
    fn finish(&mut self, copies: &mut Vec<String>) {
        let body = take(&mut self.body);
        self.state = ScanState::Ground;
        record_clipboard(&body, copies);
    }

    /// Drop a cancelled OSC.
    fn cancel(&mut self) {
        self.body.clear();
        self.state = ScanState::Ground;
    }
}

/// Record one OSC body when it is an OSC 52 clipboard write.
fn record_clipboard(body: &[u8], copies: &mut Vec<String>) {
    let Some(rest) = body.strip_prefix(b"52;") else {
        return;
    };
    let Some(split) = rest.iter().position(|byte| *byte == b';') else {
        return;
    };
    let Some((_, tail)) = rest.split_at_checked(split) else {
        return;
    };
    let Some(encoded) = tail.get(1..) else {
        return;
    };
    if encoded.is_empty() {
        copies.push(String::new());
        return;
    }
    let Ok(encoded) = std::str::from_utf8(encoded) else {
        return;
    };
    let Some(decoded) = decode_base64(encoded) else {
        return;
    };
    if let Ok(text) = String::from_utf8(decoded)
        && !text.is_empty()
    {
        copies.push(text);
    }
}

/// Decode standard base64, ignoring ASCII whitespace.
///
/// `None` means the payload is not a complete, valid base64 group.
fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut first = 0;
    let mut second = 0;
    let mut third = 0;
    let mut last = 0;
    let mut filled = 0;
    let mut padding = 0;
    for byte in encoded.bytes() {
        if is_space(byte) {
            continue;
        }
        let value = if byte == b'=' {
            if filled == 0 || padding == MAXIMUM_PADDING {
                return None;
            }
            padding = padding.checked_add(1)?;
            0
        } else {
            if padding != 0 {
                return None;
            }
            base64_value(byte)?
        };
        match filled {
            0 => first = value,
            1 => second = value,
            THIRD => third = value,
            LAST => last = value,
            _ => return None,
        }
        filled = filled.checked_add(1)?;
        if filled == GROUP_LENGTH {
            emit(&mut output, [first, second, third, last], padding)?;
            filled = 0;
            padding = 0;
        }
    }
    if filled != 0 {
        return None;
    }
    Some(output)
}

/// One sextet value, or `None` when `byte` is not in the base64 alphabet.
fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => byte.checked_sub(b'A'),
        b'a'..=b'z' => byte.checked_sub(b'a')?.checked_add(LETTERS),
        b'0'..=b'9' => byte
            .checked_sub(b'0')?
            .checked_add(LETTERS.saturating_add(LETTERS)),
        b'+' => LETTERS.checked_add(LETTERS)?.checked_add(DIGITS),
        b'/' => LETTERS
            .checked_add(LETTERS)?
            .checked_add(DIGITS)?
            .checked_add(1),
        _ => None,
    }
}

/// Whether `byte` is ASCII whitespace a base64 payload may contain.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Append the decoded bytes of one complete base64 group.
fn emit(output: &mut Vec<u8>, group: [u8; GROUP_LENGTH], padding: usize) -> Option<()> {
    let [first, second, third, last] = group;
    let high =
        u32::from(first).checked_shl(FIRST_SHIFT)? | u32::from(second).checked_shr(SECOND_SHIFT)?;
    let mid =
        u32::from(second).checked_shl(SECOND_SHIFT)? | u32::from(third).checked_shr(FIRST_SHIFT)?;
    let low = u32::from(third).checked_shl(THIRD_SHIFT)? | u32::from(last);
    let produced = DECODED_BYTES.checked_sub(padding)?;
    push_byte(output, high)?;
    if produced > 1 {
        push_byte(output, mid)?;
    }
    if produced == DECODED_BYTES {
        push_byte(output, low)?;
    }
    Some(())
}

/// Append the low eight bits of a combined sextet word.
fn push_byte(output: &mut Vec<u8>, value: u32) -> Option<()> {
    output.push(u8::try_from(value & BYTE_MASK).ok()?);
    Some(())
}
