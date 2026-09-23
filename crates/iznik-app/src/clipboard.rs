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
/// The standard base64 alphabet; a character's position is its six-bit value.
const BASE64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Bits one base64 character carries.
const CHARACTER_BITS: u32 = 6;
/// Bits in one decoded byte.
const BYTE_BITS: u32 = 8;
/// Characters in one complete base64 group, padding included.
const GROUP_LENGTH: usize = 4;
/// The padding character that fills out a final group.
const PADDING: u8 = b'=';

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
/// Each character is looked up in [`BASE64_ALPHABET`] and its six bits are
/// shifted into an pending; every time eight bits are waiting, one byte
/// comes out. Padding may only end the payload, and the payload must be whole
/// groups of four. `None` means it is not valid base64.
fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
    let characters: Vec<u8> = encoded.bytes().filter(|byte| !is_space(*byte)).collect();
    if characters.len().checked_rem(GROUP_LENGTH) != Some(0) {
        return None;
    }
    let data_length = characters
        .iter()
        .rposition(|character| *character != PADDING)
        .map_or(0, |last| last.saturating_add(1));
    let padding = characters.len().saturating_sub(data_length);
    if padding >= GROUP_LENGTH.saturating_sub(1) {
        return None;
    }
    let mut output = Vec::new();
    let mut pending: u32 = 0;
    let mut waiting: u32 = 0;
    for character in characters.get(..data_length)? {
        let value = BASE64_ALPHABET
            .iter()
            .position(|symbol| symbol == character)?;
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
