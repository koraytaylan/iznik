//! The observer proven against the sequences it recognizes: each yields one
//! event of the right kind at the right place; splitting the stream at every
//! byte yields the identical events; oversize and noise are ignored; and a real
//! `bash` with the integration asset produces the marks around one command.

use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use iznik_protocol::identity::Sequence;
use iznik_protocol::message::MarkKind;
use iznik_server::pty::spawn::{Program, SpawnOptions, spawn};
use iznik_server::terminal::marks::{MAXIMUM_OSC_LENGTH, MarkObserver};

/// The bytes of each recognized sequence, and the event it should yield.
fn cases() -> Vec<(Vec<u8>, MarkKind)> {
    vec![
        (b"\x1b]133;A\x07".to_vec(), MarkKind::PromptStart),
        (b"\x1b]133;B\x07".to_vec(), MarkKind::CommandStart),
        (b"\x1b]133;C\x07".to_vec(), MarkKind::CommandExecuted),
        (
            b"\x1b]133;D;0\x07".to_vec(),
            MarkKind::CommandFinished {
                exit_status: Some(0),
            },
        ),
        (
            b"\x1b]7;file://host/home/user\x07".to_vec(),
            MarkKind::WorkingDirectory {
                path: "/home/user".to_owned(),
            },
        ),
        (
            b"\x1b]0;A Title\x07".to_vec(),
            MarkKind::Title {
                text: "A Title".to_owned(),
            },
        ),
        (
            b"\x1b]2;A Title\x1b\\".to_vec(),
            MarkKind::Title {
                text: "A Title".to_owned(),
            },
        ),
        (
            b"\x1b]0;a;b\x07".to_vec(),
            MarkKind::Title {
                text: "a;b".to_owned(),
            },
        ),
        (
            b"\x1b[?47h".to_vec(),
            MarkKind::AlternateScreen { entered: true },
        ),
        (
            b"\x1b[?47l".to_vec(),
            MarkKind::AlternateScreen { entered: false },
        ),
        (
            b"\x1b[?1047h".to_vec(),
            MarkKind::AlternateScreen { entered: true },
        ),
        (
            b"\x1b[?1047l".to_vec(),
            MarkKind::AlternateScreen { entered: false },
        ),
        (
            b"\x1b[?1049h".to_vec(),
            MarkKind::AlternateScreen { entered: true },
        ),
        (
            b"\x1b[?1049l".to_vec(),
            MarkKind::AlternateScreen { entered: false },
        ),
    ]
}

/// The events one observer yields over a whole stream.
fn observe_whole(bytes: &[u8]) -> Vec<(Sequence, usize, MarkKind)> {
    flatten(MarkObserver::new().observe(Sequence(0), bytes))
}

/// The events one observer yields when a stream is split at `at`.
fn observe_split(bytes: &[u8], at: usize) -> Vec<(Sequence, usize, MarkKind)> {
    let mut observer = MarkObserver::new();
    let mut events = observer.observe(Sequence(0), bytes.get(..at).unwrap_or_default());
    let start = Sequence(u64::try_from(at).unwrap_or(0));
    events.extend(observer.observe(start, bytes.get(at..).unwrap_or_default()));
    flatten(events)
}

/// The observer's events as comparable tuples.
fn flatten(
    events: Vec<iznik_server::terminal::marks::MarkEvent>,
) -> Vec<(Sequence, usize, MarkKind)> {
    events
        .into_iter()
        .map(|event| (event.sequence, event.length, event.kind))
        .collect()
}

/// Each sequence yields exactly one event of the right kind, at sequence zero,
/// its length its byte count.
///
/// # Panics
///
/// When any sequence does not.
#[test]
fn shell_integration_marks_each_sequence_yields_one_event() {
    for (bytes, kind) in cases() {
        let events = observe_whole(&bytes);
        assert_eq!(events.len(), 1, "{bytes:?} yields one event");
        let (sequence, length, seen) = events.into_iter().next().expect("an event");
        assert_eq!(seen, kind, "{bytes:?} is the right kind");
        assert_eq!(sequence, Sequence(0), "at the sequence's start");
        assert_eq!(length, bytes.len(), "the whole sequence's length");
    }
}

/// A stream of every mark, split at every byte position, yields the identical
/// events — the split-at-any-byte property.
///
/// # Panics
///
/// When any split differs from the whole.
#[test]
fn shell_integration_marks_split_at_every_byte_is_identical() {
    let mut stream = Vec::new();
    for (bytes, _kind) in cases() {
        stream.extend_from_slice(b"plain ");
        stream.extend_from_slice(&bytes);
    }
    stream.extend_from_slice(b" tail");
    let whole = observe_whole(&stream);
    for at in 0..=stream.len() {
        assert_eq!(
            observe_split(&stream, at),
            whole,
            "split at {at} matches whole"
        );
    }
}

/// The event comes with the chunk that completes the sequence, never before, and
/// the observer returns only events, never bytes.
///
/// # Panics
///
/// When the event does not arrive with the completing chunk.
#[test]
fn shell_integration_marks_events_come_with_the_completing_chunk() {
    let mut observer = MarkObserver::new();
    assert!(
        observer.observe(Sequence(0), b"\x1b]133;A").is_empty(),
        "no event before the terminator"
    );
    let completing = observer.observe(Sequence(7), b"\x07");
    assert_eq!(
        completing.len(),
        1,
        "the event comes with the completing chunk"
    );
    let event = completing.into_iter().next().expect("an event");
    assert_eq!(event.kind, MarkKind::PromptStart);
    assert_eq!(event.sequence, Sequence(0), "at the sequence's start");
    assert_eq!(event.length, 8, "the whole sequence's length");
}

/// An OSC longer than the cap yields no event, and the next well-formed mark is
/// still recognized.
///
/// # Panics
///
/// When the oversize OSC yields an event, or the next mark is lost.
#[test]
fn shell_integration_marks_an_oversize_osc_is_abandoned() {
    let mut stream = Vec::new();
    stream.extend_from_slice(b"\x1b]0;");
    stream.extend(std::iter::repeat_n(
        b'x',
        MAXIMUM_OSC_LENGTH.saturating_add(16),
    ));
    stream.extend_from_slice(b"\x07");
    stream.extend_from_slice(b"\x1b]133;A\x07");
    let events = observe_whole(&stream);
    assert_eq!(events.len(), 1, "only the mark after the oversize OSC");
    let (_sequence, _length, kind) = events.into_iter().next().expect("an event");
    assert_eq!(kind, MarkKind::PromptStart);
}

/// Noise is ignored without error — including the exact payloads a naive parser
/// aborts on: the vte `666` mark `bash` emits, an empty OSC, and a code with no
/// body — alongside a `D` with no status, a non-`file://` OSC 7, an unrelated
/// OSC, and private-mode CSIs that are not the three switches.
///
/// # Panics
///
/// When any noise yields an event or is not survived.
#[test]
fn shell_integration_marks_noise_is_ignored() {
    let noises: &[&[u8]] = &[
        b"\x1b]666;vte.shell.preexec!\x1b\\",
        b"\x1b]\x07",
        b"\x1b]133\x07",
        b"\x1b]133;D\x07",
        b"\x1b]7;http://host/where\x07",
        b"\x1b]52;c;encoded\x07",
        b"\x1b[?25h",
        b"\x1b[2J",
    ];
    for noise in noises {
        assert!(observe_whole(noise).is_empty(), "{noise:?} is ignored");
    }
}

/// A mark whose first byte is at a non-zero absolute sequence reports that
/// absolute sequence, not a chunk-relative offset.
///
/// # Panics
///
/// When the reported sequence is not the absolute one.
#[test]
fn shell_integration_marks_report_absolute_sequences() {
    let events = MarkObserver::new().observe(Sequence(1000), b"\x1b]133;A\x07");
    let event = events.into_iter().next().expect("an event");
    assert_eq!(event.sequence, Sequence(1000), "the absolute sequence");
    assert_eq!(event.length, 8, "the sequence's length");
}

/// A mark that follows a sequence the program left unterminated — ended only by
/// the escape of the next sequence — is still recognized: an OSC so ended is
/// dispatched at its last byte, and an incomplete CSI is dropped.
///
/// # Panics
///
/// When a mark after an unterminated sequence is lost.
#[test]
fn shell_integration_marks_a_mark_after_an_unterminated_sequence_survives() {
    let cases: &[(&[u8], &[MarkKind])] = &[
        (
            b"\x1b]0;busy\x1b]133;A\x07",
            &[
                MarkKind::Title {
                    text: "busy".to_owned(),
                },
                MarkKind::PromptStart,
            ],
        ),
        (b"\x1b[?47\x1b]133;A\x07", &[MarkKind::PromptStart]),
        (
            b"\x1b]0;x\x1b]133;D;130\x07",
            &[
                MarkKind::Title {
                    text: "x".to_owned(),
                },
                MarkKind::CommandFinished {
                    exit_status: Some(130),
                },
            ],
        ),
    ];
    for &(bytes, expected) in cases {
        let kinds: Vec<MarkKind> = MarkObserver::new()
            .observe(Sequence(0), bytes)
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert_eq!(kinds.as_slice(), expected, "{bytes:?}");
    }
}

/// A reader draining a descriptor on its own thread, so it can be read to a
/// quiet without blocking.
struct Reader {
    /// Chunks seen.
    chunks: mpsc::Receiver<Vec<u8>>,
    /// The reader thread.
    _thread: thread::JoinHandle<()>,
}

impl Reader {
    /// A reader over `source`.
    fn new(mut source: Box<dyn Read + Send>) -> Reader {
        let (sender, chunks) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut buffer = vec![0; 4096];
            while let Ok(count) = source.read(&mut buffer) {
                let chunk = buffer.get(..count).unwrap_or_default().to_vec();
                if count == 0 || sender.send(chunk).is_err() {
                    return;
                }
            }
        });
        Reader {
            chunks,
            _thread: thread,
        }
    }

    /// Everything seen until a quarter-second of quiet or two seconds elapse.
    fn drain(&self) -> Vec<u8> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < Duration::from_secs(2) {
            match self.chunks.recv_timeout(Duration::from_millis(250)) {
                Ok(chunk) => seen.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) if !seen.is_empty() => break,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        seen
    }
}

/// A real `bash` with the integration asset, given one command, emits the four
/// OSC 133 marks and a working-directory report through the observer.
///
/// # Panics
///
/// When any of the marks is missing.
#[test]
fn shell_integration_marks_the_asset_emits_the_marks() {
    let asset = format!(
        "{}/../iznik-testkit/assets/shell-integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    let options = SpawnOptions {
        program: Program::Command {
            path: "bash".into(),
            arguments: vec!["--rcfile".to_owned(), asset, "-i".to_owned()],
        },
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
    };
    let process = spawn(&options).expect("bash starts");
    let reader = Reader::new(process.master().try_clone_reader().expect("a reader"));
    let mut writer = process.master().take_writer().expect("a writer");
    thread::sleep(Duration::from_millis(400));
    writeln!(writer, "true").expect("a command is written");
    thread::sleep(Duration::from_millis(300));
    writeln!(writer, "exit").expect("exit is written");
    let output = reader.drain();
    let kinds: Vec<MarkKind> = MarkObserver::new()
        .observe(Sequence(0), &output)
        .into_iter()
        .map(|event| event.kind)
        .collect();
    for expected in [
        MarkKind::PromptStart,
        MarkKind::CommandStart,
        MarkKind::CommandExecuted,
        MarkKind::CommandFinished {
            exit_status: Some(0),
        },
    ] {
        assert!(
            kinds.contains(&expected),
            "{expected:?} was emitted, saw {kinds:?}"
        );
    }
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, MarkKind::WorkingDirectory { .. })),
        "a working directory was reported, saw {kinds:?}"
    );
}
