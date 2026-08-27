//! The shell-integration observer: OSC 133, OSC 7, titles and alternate-screen
//! switches recognized as bytes pass, across any chunk boundary, without
//! touching them.
//!
//! The framing scan — where an OSC or CSI begins and ends — is this module's,
//! and it survives a sequence split across reads at any byte, the bug every
//! naive implementation of this has. It follows the one ANSI rule a naive scan
//! forgets: an escape restarts the scan anywhere, so an OSC that a program left
//! unterminated is dispatched at its last byte and the mark that follows it is
//! never swallowed. (An 8-bit `ST`, `0x9c`, is not a terminator here: a pane's
//! bytes are UTF-8 under `TERM=xterm-ghostty`, where `0x9c` is a continuation
//! byte, not a control.)
//!
//! The plan hands each completed OSC to `libghostty_vt::osc::Parser`, but its
//! 0.2.1 release cannot serve that role: a server cannot panic on what a program
//! prints, and `end` returns a null command — an internal panic — for the vte
//! `666` mark `bash` actually emits, for an empty or unterminated payload, and
//! for any payload past a few kilobytes including a valid long title; on top of
//! that its title extraction always yields an empty string, and its
//! `CommandType` does not carry the OSC 133 kind at all. So this module reads
//! the small, fixed OSC grammar it needs — the code, the semantic-prompt kind,
//! the directory, the title — straight from the payload it already framed, and
//! never lets a payload reach a parser that would abort on it.

use iznik_protocol::identity::Sequence;
use iznik_protocol::message::MarkKind;

/// The most bytes an OSC may carry before it is abandoned; its bytes still pass.
pub const MAXIMUM_OSC_LENGTH: usize = 4 * 1024;

/// The escape that begins every sequence.
const ESCAPE: u8 = 0x1b;

/// The bell that ends an OSC.
const BELL: u8 = 0x07;

/// The byte after escape that begins an OSC.
const OSC_INTRODUCER: u8 = b']';

/// The byte after escape that begins a CSI.
const CSI_INTRODUCER: u8 = b'[';

/// The byte after escape that ends an OSC as a string terminator.
const STRING_TERMINATOR: u8 = b'\\';

/// The lowest byte that ends a CSI.
const CSI_FINAL_MINIMUM: u8 = 0x40;

/// The highest byte that ends a CSI.
const CSI_FINAL_MAXIMUM: u8 = 0x7e;

/// The lowest byte that continues a CSI as a parameter or intermediate.
const CSI_PARAMETER_MINIMUM: u8 = 0x20;

/// The highest byte that continues a CSI as a parameter or intermediate.
const CSI_PARAMETER_MAXIMUM: u8 = 0x3f;

/// The hex digits in a `%XX` percent-escape.
const PERCENT_ESCAPE_DIGITS: usize = 2;

/// The radix of a percent-escape's hex digits.
const HEXADECIMAL_RADIX: u32 = 16;

/// One recognized mark: the wire kind, the absolute sequence of its first byte,
/// and its byte length, so a pane can split a chunk exactly at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkEvent {
    /// The absolute sequence of the sequence's first byte.
    pub sequence: Sequence,
    /// The sequence's byte length.
    pub length: usize,
    /// What the mark means.
    pub kind: MarkKind,
}

/// Where the scan is within a sequence.
#[derive(Clone, Copy, Debug)]
enum Scan {
    /// Not in a sequence.
    Ground,
    /// The escape has been seen.
    Escape,
    /// Inside an OSC, gathering its payload.
    Osc,
    /// Inside an OSC, an escape seen — a string terminator if a backslash next.
    OscEscape,
    /// Inside a CSI, gathering its parameters.
    Csi,
}

/// A pass-through scanner turning shell-integration sequences into events as the
/// bytes go by, without modifying, delaying or reordering them.
#[derive(Clone, Debug)]
pub struct MarkObserver {
    /// Where the scan is.
    scan: Scan,
    /// The bytes of the sequence in progress, after its introducer.
    payload: Vec<u8>,
    /// The absolute sequence of the sequence in progress's first byte.
    start: u64,
    /// Whether the sequence in progress has passed the length cap.
    overflow: bool,
}

impl Default for MarkObserver {
    fn default() -> MarkObserver {
        MarkObserver::new()
    }
}

impl MarkObserver {
    /// A fresh observer in the ground state.
    #[must_use]
    pub fn new() -> MarkObserver {
        MarkObserver {
            scan: Scan::Ground,
            payload: Vec::new(),
            start: 0,
            overflow: false,
        }
    }

    /// Scans `bytes`, whose first byte is at absolute `sequence`, and returns
    /// the marks that complete within them. The bytes are never modified.
    pub fn observe(&mut self, sequence: Sequence, bytes: &[u8]) -> Vec<MarkEvent> {
        let mut events = Vec::new();
        for (offset, &byte) in bytes.iter().enumerate() {
            let position = sequence
                .0
                .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX));
            self.step(byte, position, &mut events);
        }
        events
    }

    /// Advances the scan by one byte, pushing any completed mark.
    fn step(&mut self, byte: u8, position: u64, events: &mut Vec<MarkEvent>) {
        match self.scan {
            Scan::Ground => {
                if byte == ESCAPE {
                    self.begin(position);
                }
            }
            Scan::Escape => self.after_escape(byte, position),
            Scan::Osc => self.in_osc(byte, position, events),
            Scan::OscEscape => self.in_osc_escape(byte, position, events),
            Scan::Csi => self.in_csi(byte, position, events),
        }
    }

    /// Begins a new sequence at an escape.
    fn begin(&mut self, position: u64) {
        self.scan = Scan::Escape;
        self.start = position;
        self.payload.clear();
        self.overflow = false;
    }

    /// The byte after an escape: an OSC, a CSI, another escape, or nothing.
    fn after_escape(&mut self, byte: u8, position: u64) {
        match byte {
            OSC_INTRODUCER => self.scan = Scan::Osc,
            CSI_INTRODUCER => self.scan = Scan::Csi,
            ESCAPE => self.begin(position),
            _ => self.scan = Scan::Ground,
        }
    }

    /// A byte inside an OSC.
    fn in_osc(&mut self, byte: u8, position: u64, events: &mut Vec<MarkEvent>) {
        match byte {
            BELL => self.finish_osc(position, events),
            ESCAPE => self.scan = Scan::OscEscape,
            _ => self.gather(byte),
        }
    }

    /// A byte inside an OSC after an escape. A backslash makes an `ESC \` string
    /// terminator, both bytes part of the mark. Any other byte means the escape
    /// was not a terminator: it ends this OSC at its last content byte and itself
    /// begins the next sequence, so the mark after an unterminated OSC survives.
    fn in_osc_escape(&mut self, byte: u8, position: u64, events: &mut Vec<MarkEvent>) {
        if byte == STRING_TERMINATOR {
            self.finish_osc(position, events);
        } else {
            let escape = position.saturating_sub(1);
            self.dispatch_osc(escape.saturating_sub(1), events);
            self.begin(escape);
            self.after_escape(byte, position);
        }
    }

    /// A byte inside a CSI: a final byte ends it, a parameter continues it, an
    /// escape restarts the scan, anything else abandons it.
    fn in_csi(&mut self, byte: u8, position: u64, events: &mut Vec<MarkEvent>) {
        if (CSI_FINAL_MINIMUM..=CSI_FINAL_MAXIMUM).contains(&byte) {
            if let Some(kind) = alternate_screen(&self.payload, byte) {
                events.push(self.event(position, kind));
            }
            self.scan = Scan::Ground;
        } else if (CSI_PARAMETER_MINIMUM..=CSI_PARAMETER_MAXIMUM).contains(&byte) {
            self.gather(byte);
        } else if byte == ESCAPE {
            self.begin(position);
        } else {
            self.scan = Scan::Ground;
        }
    }

    /// Gathers one payload byte, marking an overflow past the cap.
    fn gather(&mut self, byte: u8) {
        if self.payload.len() >= MAXIMUM_OSC_LENGTH {
            self.overflow = true;
        }
        if !self.overflow {
            self.payload.push(byte);
        }
    }

    /// Dispatches the OSC gathered so far as a mark ending at `end`, when it is
    /// one and did not overflow; leaves the scan state untouched.
    fn dispatch_osc(&mut self, end: u64, events: &mut Vec<MarkEvent>) {
        if !self.overflow
            && let Some(kind) = classify_osc(&self.payload)
        {
            events.push(self.event(end, kind));
        }
    }

    /// Finishes an OSC at its terminator, returning the scan to ground.
    fn finish_osc(&mut self, position: u64, events: &mut Vec<MarkEvent>) {
        self.dispatch_osc(position, events);
        self.scan = Scan::Ground;
    }

    /// The event for a sequence that ends at `position`.
    fn event(&self, position: u64, kind: MarkKind) -> MarkEvent {
        let length = position.saturating_sub(self.start).saturating_add(1);
        MarkEvent {
            sequence: Sequence(self.start),
            length: usize::try_from(length).unwrap_or(usize::MAX),
            kind,
        }
    }
}

/// The mark an OSC payload carries: its leading numeric code chooses the grammar
/// for the body, and only the four mark codes yield an event.
fn classify_osc(payload: &[u8]) -> Option<MarkKind> {
    let text = std::str::from_utf8(payload).ok()?;
    let (code, body) = text.split_once(';')?;
    match code {
        "133" => semantic_prompt(body),
        "7" => report_pwd(body),
        "0" | "2" => Some(MarkKind::Title {
            text: body.to_owned(),
        }),
        _other => None,
    }
}

/// The semantic-prompt mark of a `133;` body: `A`/`B`/`C`, or `D;<status>` with
/// a numeric status; anything else is not a mark this observer reports.
fn semantic_prompt(body: &str) -> Option<MarkKind> {
    let mut fields = body.split(';');
    match fields.next()? {
        "A" => Some(MarkKind::PromptStart),
        "B" => Some(MarkKind::CommandStart),
        "C" => Some(MarkKind::CommandExecuted),
        "D" => {
            let status = fields.next()?.parse().ok()?;
            Some(MarkKind::CommandFinished {
                exit_status: Some(status),
            })
        }
        _other => None,
    }
}

/// The working directory of a `7;` body, or none when it is not a `file://` URL
/// with a path. The path is percent-decoded, as its `file://` URL encodes it.
fn report_pwd(body: &str) -> Option<MarkKind> {
    let url = body.strip_prefix("file://")?;
    let path = url.get(url.find('/')?..)?;
    Some(MarkKind::WorkingDirectory {
        path: percent_decoded(path),
    })
}

/// A `file://` path with its `%XX` escapes decoded, leaving anything that is not
/// a valid escape, or that does not decode to text, as written.
fn percent_decoded(path: &str) -> String {
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'%'
            && let Some(hex) = tail.get(..PERCENT_ESCAPE_DIGITS)
            && let Ok(text) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(text, HEXADECIMAL_RADIX)
        {
            bytes.push(byte);
            rest = tail.get(PERCENT_ESCAPE_DIGITS..).unwrap_or_default();
        } else {
            bytes.push(first);
            rest = tail;
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_error| path.to_owned())
}

/// The alternate-screen switch a CSI carries, or none when it is not one of the
/// three switches.
fn alternate_screen(payload: &[u8], final_byte: u8) -> Option<MarkKind> {
    let entered = match final_byte {
        b'h' => true,
        b'l' => false,
        _other => return None,
    };
    match std::str::from_utf8(payload).ok()? {
        "?47" | "?1047" | "?1049" => Some(MarkKind::AlternateScreen { entered }),
        _other => None,
    }
}
