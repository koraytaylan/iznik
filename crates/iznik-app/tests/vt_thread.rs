//! The committed fidelity corpus through the application's owning VT thread.

use iznik_app::bridge::EngineBridge;
use iznik_app::vt::{
    PaneKey, TerminalSnapshot, TerminalTheme, VtCommand, VtError, VtOptions, VtThread,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::Sequence;
use iznik_testkit::{
    corpus,
    vt::{Color, Vt, Width},
};
use libghostty_vt::render::Dirty;
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::RgbColor;

mod support;
use support::{key, open, receive, snapshot};

/// Compare text, dimensions, cursor and history with the independent oracle adapter.
///
/// # Errors
/// Returns oracle read or coordinate conversion failures.
///
/// # Panics
/// Fails when any visible cell or terminal state differs from the oracle.
fn assert_snapshot(
    actual: &TerminalSnapshot,
    oracle: &Vt,
) -> Result<(), Box<dyn std::error::Error>> {
    let (columns, rows) = oracle.size()?;
    assert_eq!(actual.columns, columns, "live width");
    assert_eq!(actual.rows.len(), usize::from(rows), "live height");
    for (row_index, row) in actual.rows.iter().enumerate() {
        for (column_index, cell) in row.iter().enumerate() {
            let expected = oracle.cell(u16::try_from(column_index)?, u16::try_from(row_index)?)?;
            assert_cell(cell, &expected, &actual.colors);
        }
    }
    if let Some(cursor) = &actual.cursor {
        assert_eq!((cursor.x, cursor.y), oracle.cursor()?, "cursor position");
    }
    assert_eq!(
        actual.alternate,
        oracle.in_alternate_screen()?,
        "active screen"
    );
    assert_eq!(
        actual.scrollback_rows,
        oracle.scrollback_rows()?,
        "history extent"
    );
    Ok(())
}

/// Compare one cell including background-only cells and wide continuations.
///
/// # Panics
/// Fails if the renderer snapshot differs from the independent oracle adapter.
fn assert_cell(
    cell: &iznik_app::vt::CellSnapshot,
    expected: &iznik_testkit::vt::Cell,
    colors: &libghostty_vt::render::Colors,
) {
    assert_eq!(
        Some(cell.foreground),
        resolve(expected.foreground, colors.foreground, &colors.palette),
        "foreground"
    );
    assert_eq!(
        Some(cell.background),
        resolve(expected.background, colors.background, &colors.palette),
        "background"
    );
    let width = match cell.width {
        CellWide::Wide => Width::Wide,
        CellWide::SpacerTail => Width::Continuation,
        CellWide::Narrow | CellWide::SpacerHead => Width::Narrow,
    };
    assert_eq!(width, expected.width, "cell width");
    assert_eq!(cell.text, expected.grapheme, "grapheme");
    assert_eq!(cell.style.bold, expected.bold, "bold");
    assert_eq!(cell.style.italic, expected.italic, "italic");
    assert_eq!(
        format!("{:?}", cell.style.underline).to_lowercase(),
        expected.underline.to_string(),
        "underline"
    );
}

/// Resolve oracle colors using the snapshot's active palette.
fn resolve(color: Color, default: RgbColor, palette: &[RgbColor]) -> Option<RgbColor> {
    match color {
        Color::Default => Some(default),
        Color::Palette(index) => palette.get(usize::from(index)).copied(),
        Color::Rgb { red, green, blue } => Some(RgbColor {
            r: red,
            g: green,
            b: blue,
        }),
    }
}

/// Prove the named thread contract through the owning thread.
///
/// # Panics
/// Fails if a thread reply differs from the asserted contract.
#[test]
fn corpus_snapshots_match_the_oracle() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    for construct in corpus::constructs() {
        let mut oracle = Vt::new(80, 24).expect("oracle");
        let mut sequence = Sequence(0);
        open(&thread, sequence, 80, 24).expect("open");
        for bytes in construct.chunks {
            oracle.feed(&bytes);
            let length = u64::try_from(bytes.len()).expect("length");
            thread
                .send(VtCommand::Feed {
                    receipt: None,
                    key: key(),
                    sequence,
                    bytes,
                })
                .expect("feed");
            sequence = Sequence(sequence.0.checked_add(length).expect("sequence"));
            let actual = snapshot(&thread).expect("snapshot");
            assert_eq!(actual.sequence, sequence, "{}", construct.name);
            assert_eq!(u64::from(actual.consumed_bytes), length);
            assert_snapshot(&actual, &oracle).expect("oracle comparison");
        }
    }
}

/// Prove the named thread contract through the owning thread.
///
/// # Panics
/// Fails if a thread reply differs from the asserted contract.
#[test]
fn gap_needs_a_fresh_screen_and_does_not_apply_bytes() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(100), 10, 3).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(101),
            bytes: b"bad".to_vec(),
        })
        .expect("gap");
    assert!(matches!(receive(&thread).result, Err(VtError::NeedsScreen)));
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(100),
            bytes: b"also bad".to_vec(),
        })
        .expect("retry");
    assert!(matches!(receive(&thread).result, Err(VtError::NeedsScreen)));
    let recovered = open(&thread, Sequence(200), 10, 3).expect("open");
    assert_eq!(recovered.sequence, Sequence(200));
    assert_eq!(recovered.consumed_bytes, 0);
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(200),
            bytes: b"ok".to_vec(),
        })
        .expect("feed");
    assert_eq!(snapshot(&thread).expect("snapshot").sequence, Sequence(202));
}

/// Prove the named thread contract through the owning thread.
///
/// # Panics
/// Fails if a thread reply differs from the asserted contract.
#[test]
fn client_answers_queries_with_its_theme_and_size() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 80, 24).expect("open");
    let theme = TerminalTheme {
        background: RgbColor {
            r: 17,
            g: 34,
            b: 51,
        },
        ..TerminalTheme::default()
    };
    thread
        .send(VtCommand::Theme {
            key: key(),
            theme: Box::new(theme),
        })
        .expect("theme");
    assert_eq!(
        snapshot(&thread).expect("snapshot").colors.background,
        RgbColor {
            r: 17,
            g: 34,
            b: 51
        }
    );
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(0),
            bytes: b"\x1b[6n\x1b[c\x1b[>q\x1b[18t\x1b]11;?\x07".to_vec(),
        })
        .expect("query");
    let reply = String::from_utf8(snapshot(&thread).expect("snapshot").responses).expect("reply");
    assert!(reply.contains("\x1b[1;1R"), "{reply:?}");
    assert!(reply.contains("\x1b[?62c"), "{reply:?}");
    assert!(reply.contains("iznik-app"), "{reply:?}");
    assert!(reply.contains("\x1b[8;24;80t"), "{reply:?}");
    assert!(reply.contains("rgb:1111/2222/3333"), "{reply:?}");
}

/// Prove the named thread contract through the owning thread.
///
/// # Panics
/// Fails if a thread reply differs from the asserted contract.
#[test]
fn resize_alternate_screen_and_idle_damage_follow_the_emulator() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 10, 3).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(0),
            bytes: b"primary\x1b[?1049halternate".to_vec(),
        })
        .expect("alternate");
    let alternate = snapshot(&thread).expect("snapshot");
    assert!(alternate.alternate);
    thread
        .send(VtCommand::Resize {
            key: key(),
            columns: 20,
            rows: 5,
        })
        .expect("resize");
    let resized = snapshot(&thread).expect("snapshot");
    assert_eq!(resized.columns, 20);
    assert_eq!(resized.rows.len(), 5);
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: alternate.sequence,
            bytes: b"\x1b[?1049l".to_vec(),
        })
        .expect("primary");
    assert!(!snapshot(&thread).expect("snapshot").alternate);
    thread.send(VtCommand::Snapshot(key())).expect("idle");
    let idle = snapshot(&thread).expect("snapshot");
    assert_eq!(idle.dirty, Dirty::Clean);
    assert!(idle.dirty_rows.iter().all(|dirty| !dirty));
    thread.send(VtCommand::Close(key())).expect("close");
    assert!(receive(&thread).result.expect("close result").is_none());
    thread
        .send(VtCommand::Snapshot(key()))
        .expect("closed snapshot");
    assert!(matches!(receive(&thread).result, Err(VtError::NeedsScreen)));
}

/// Host-local pane numbers must never alias across remote hosts.
///
/// # Panics
/// Fails when a screen replacement or output crosses host boundaries.
#[test]
fn hosts_keep_independent_sequences_and_screen_replay_has_no_effects() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(50), 20, 4).expect("first host");
    let other = PaneKey {
        host: HostId("other".to_owned()),
        pane: key().pane,
    };
    thread
        .send(VtCommand::Screen {
            key: other.clone(),
            sequence: Sequence(100),
            columns: 20,
            rows: 4,
            bytes: b"actual\x1b[6n".to_vec(),
            theme: Box::default(),
        })
        .expect("second host");
    let actual = snapshot(&thread).expect("actual");
    assert_eq!(actual.key, other);
    assert!(actual.responses.is_empty());
    assert_eq!(actual.consumed_bytes, 0);
    EngineBridge::feed_terminal(
        &thread,
        &ManagerEvent::Bytes {
            host: key().host,
            pane: key().pane,
            sequence: Sequence(50),
            bytes: b"first".to_vec(),
            receipt: None,
        },
        &TerminalTheme::default(),
    )
    .expect("bridge feed");
    let first = snapshot(&thread).expect("first output");
    assert_eq!(first.key, key());
    assert_eq!(first.sequence, Sequence(55));
    assert_eq!(first.consumed_bytes, 5);
    thread
        .send(VtCommand::Snapshot(other))
        .expect("other snapshot");
    assert_eq!(
        snapshot(&thread).expect("other state").sequence,
        Sequence(100)
    );
}

/// Theme defaults must survive reset without replacing program OSC overrides.
///
/// # Panics
/// Fails when the emulator reports colors other than its effective defaults.
#[test]
fn theme_updates_keep_the_program_color_override_until_reset() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 10, 3).expect("open");
    let bytes = b"\x1b]11;#aabbcc\x07".to_vec();
    let next = Sequence(u64::try_from(bytes.len()).expect("length"));
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(0),
            bytes,
        })
        .expect("override");
    snapshot(&thread).expect("override state");
    let background = RgbColor { r: 1, g: 2, b: 3 };
    let theme = TerminalTheme {
        background,
        ..TerminalTheme::default()
    };
    thread
        .send(VtCommand::Theme {
            key: key(),
            theme: Box::new(theme),
        })
        .expect("theme");
    assert_eq!(
        snapshot(&thread)
            .expect("overridden theme")
            .colors
            .background,
        RgbColor {
            r: 170,
            g: 187,
            b: 204
        }
    );
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: next,
            bytes: b"\x1b]111\x07".to_vec(),
        })
        .expect("reset color");
    assert_eq!(
        snapshot(&thread).expect("reset state").colors.background,
        background
    );
}
