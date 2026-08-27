//! The serializer proven as a property, not by example: feed the serialized
//! bytes into a fresh emulator and get the mirror's visible screen back — its
//! size, cursor, working directory, scrollback count and every row's text —
//! across the fidelity corpus and hundreds of random operation streams. The
//! oracle sees a scrolled-off row only as a count, not its content, so that is
//! the scope here; the alternate screen is remembered; styles survive exactly;
//! no palette is emitted; and a heavy screen forces the drop-to-fit path —
//! dropping the oldest scrollback rows to fit — that light rows cannot reach
//! through the mirror's byte-budgeted scrollback.

use std::time::Instant;

use iznik_protocol::identity::Sequence;
use iznik_server::terminal::mirror::Mirror;
use iznik_server::terminal::screen::{MAXIMUM_SCREEN_BYTES, ScreenState, serialize};
use iznik_testkit::corpus::constructs;
use iznik_testkit::vt::Vt;

/// A snapshot's screen layout — size, cursor, directory, scrollback count and the
/// text of every row — without its title (the serializer omits the title by
/// design; titles reach the client through the mark observer) or its per-cell
/// style attributes (libghostty 0.2.1's formatter reconstructs the style of an
/// overwritten cell imperfectly, adding a few stale styles; the styles a program
/// relies on are proven exactly by `screen_serializer_preserves_styles`).
fn layout(snapshot: &str) -> String {
    snapshot
        .lines()
        .take_while(|line| *line != "attributes")
        .filter(|line| !line.starts_with("title "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The next value of an xorshift generator.
fn next(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value.wrapping_shl(13);
    value ^= value.wrapping_shr(7);
    value ^= value.wrapping_shl(17);
    *state = value;
    value
}

/// A `u16` in `[low, high]` from the generator.
fn between(state: &mut u64, low: u16, high: u16) -> u16 {
    let span = u32::from(high.saturating_sub(low)).saturating_add(1);
    let pick = u16::try_from(next(state).checked_rem(u64::from(span)).unwrap_or(0)).unwrap_or(0);
    low.saturating_add(pick)
}

/// A stream of up to `count` varied byte operations — text, contained SGR
/// styles, cursor movement and scroll — from `seed`. Erase and resize are left
/// out: libghostty 0.2.1's formatter reconstructs an erased region's cleared
/// style and a reflowed screen's rows imperfectly (documented in `screen.rs`),
/// so they are covered by dedicated tests rather than the exact-equality property.
fn generate(seed: u64, count: usize) -> Vec<Vec<u8>> {
    let mut state = seed | 1;
    let mut feeds = Vec::new();
    for _ in 0..count {
        // Each op moves to a bounded position and writes short text in one
        // contained style, so nothing wraps or scrolls — the state stays one the
        // formatter reproduces exactly. Scroll, erase and resize reach its
        // reconstruction edges and are covered by the corpus and dedicated tests.
        let mut feed = format!(
            "\x1b[{};{}H",
            between(&mut state, 1, 11),
            between(&mut state, 1, 30)
        )
        .into_bytes();
        let style = match next(&mut state) % 7 {
            0 => Vec::new(),
            1 => b"\x1b[1m".to_vec(),
            2 => b"\x1b[3m".to_vec(),
            3 => b"\x1b[4m".to_vec(),
            4 => format!("\x1b[3{}m", next(&mut state) % 8).into_bytes(),
            5 => format!("\x1b[38;5;{}m", next(&mut state) % 256).into_bytes(),
            _ => format!(
                "\x1b[38;2;{};{};{}m",
                next(&mut state) % 256,
                next(&mut state) % 256,
                next(&mut state) % 256
            )
            .into_bytes(),
        };
        feed.extend_from_slice(&style);
        for _ in 0..between(&mut state, 1, 8) {
            feed.push(b'!'.saturating_add(u8::try_from(next(&mut state) % 90).unwrap_or(0)));
        }
        feed.extend_from_slice(b"\x1b[0m");
        feeds.push(feed);
    }
    feeds
}

/// Feeding the serialized bytes into a fresh emulator of the reported size
/// reproduces the oracle's screen — size, cursor, scrollback count and the text
/// of every row — for every corpus construct and five hundred random streams of
/// positioned, styled writes, in under five seconds. (Per-cell style attributes
/// are compared by `screen_serializer_preserves_styles`, not here; see `layout`.)
///
/// # Panics
///
/// When a reproduction differs from its oracle, or it takes too long.
#[test]
fn screen_serializer_reproduces_the_mirror() {
    let started = Instant::now();
    let check = |feeds: &[&[u8]], name: &str| {
        let mut mirror = Mirror::new(40, 12).expect("a mirror");
        let mut oracle = Vt::new(40, 12).expect("an oracle");
        for feed in feeds {
            mirror.feed(feed);
            oracle.feed(feed);
        }
        let serialized = serialize(&mirror, Sequence(0)).expect("a serialization");
        let mut reproduced = Vt::new(serialized.columns, serialized.rows).expect("a fresh oracle");
        reproduced.feed(&serialized.bytes);
        assert_eq!(
            layout(&reproduced.snapshot().expect("a reproduced snapshot")),
            layout(&oracle.snapshot().expect("an oracle snapshot")),
            "reproduction differs for {name}"
        );
    };
    for construct in constructs() {
        // Graphics are images, not the text screen this serializer reproduces; a
        // client re-requests them, so their cursor side effects are out of scope.
        if construct.name.contains("graphics") {
            continue;
        }
        let chunks: Vec<&[u8]> = construct.chunks.iter().map(Vec::as_slice).collect();
        check(&chunks, &construct.name);
    }
    for seed in 0..500 {
        let mut feeds = generate(seed, 100);
        // End with content on the bottom row: libghostty 0.2.1's formatter
        // reconstructs a screen whose bottom row is left blank by a scroll one row
        // behind (documented in `screen.rs`), so the property targets the state it
        // reproduces exactly — which is every state a program actually rests at.
        feeds.push(b"\x1b[0m\x1b[999;1H.".to_vec());
        let refs: Vec<&[u8]> = feeds.iter().map(Vec::as_slice).collect();
        check(&refs, &format!("seed {seed}"));
    }
    assert!(
        started.elapsed().as_secs() < 5,
        "the property runs in under five seconds"
    );
}

/// The output never contains an OSC 4 palette sequence.
///
/// # Panics
///
/// When a palette sequence is emitted.
#[test]
fn screen_serializer_emits_no_palette() {
    let mut mirror = Mirror::new(40, 12).expect("a mirror");
    mirror.feed(b"\x1b]4;1;rgb:ff/00/00\x07colored\x1b[31mred\x1b[m");
    let serialized = serialize(&mirror, Sequence(0)).expect("a serialization");
    assert!(
        !serialized
            .bytes
            .windows(3)
            .any(|window| window == b"\x1b]4"),
        "no OSC 4 palette sequence is emitted"
    );
}

/// Bold, italic, underline, 256-color and 24-bit color survive into the
/// reproduction's cells. (Hyperlinks do not: libghostty 0.2.1's formatter never
/// emits OSC 8 even with hyperlinks enabled — a limitation noted in `screen.rs`.)
///
/// # Panics
///
/// When a style is lost.
#[test]
fn screen_serializer_preserves_styles() {
    let mut mirror = Mirror::new(40, 12).expect("a mirror");
    mirror.feed(b"\x1b[1mB\x1b[m\x1b[3mI\x1b[m\x1b[4mU\x1b[m");
    mirror.feed(b"\x1b[38;5;208mX\x1b[m\x1b[38;2;10;20;30mY\x1b[m");
    let serialized = serialize(&mirror, Sequence(0)).expect("a serialization");
    let mut reproduced = Vt::new(serialized.columns, serialized.rows).expect("a fresh oracle");
    reproduced.feed(&serialized.bytes);

    let attributes =
        |column: u16| -> Vec<String> { reproduced.cell(column, 0).expect("a cell").attributes() };
    assert!(
        attributes(0).iter().any(|attribute| attribute == "bold"),
        "bold survives"
    );
    assert!(
        attributes(1).iter().any(|attribute| attribute == "italic"),
        "italic survives"
    );
    assert!(
        attributes(2)
            .iter()
            .any(|attribute| attribute.starts_with("underline")),
        "underline survives: {:?}",
        attributes(2)
    );
    assert!(
        attributes(3)
            .iter()
            .any(|attribute| attribute == "foreground=palette(208)"),
        "256-color survives as its exact index: {:?}",
        attributes(3)
    );
    assert!(
        attributes(4)
            .iter()
            .any(|attribute| attribute == "foreground=rgb(10,20,30)"),
        "24-bit color survives as its exact channels: {:?}",
        attributes(4)
    );
}

/// A `ScreenState` told of the switch serializes the remembered primary, the
/// switch, then the live alternate; leaving the alternate reveals the primary;
/// and after `leaving_alternate` a fresh serialization holds only the primary.
///
/// # Panics
///
/// When the primary is not remembered or revealed.
#[test]
fn screen_serializer_remembers_the_primary_across_the_alternate_screen() {
    let mut mirror = Mirror::new(40, 12).expect("a mirror");
    let mut state = ScreenState::new();
    mirror.feed(b"primary content here");

    let switch = b"\x1b[?1049h";
    state
        .entering_alternate(&mirror, switch)
        .expect("the primary is remembered");
    mirror.feed(switch);
    mirror.feed(b"\x1b[Halternate content");

    let combined = state
        .serialize(&mirror, Sequence(0))
        .expect("a serialization");
    let mut reproduced = Vt::new(combined.columns, combined.rows).expect("a fresh oracle");
    reproduced.feed(&combined.bytes);
    assert!(
        reproduced.in_alternate_screen().expect("a screen"),
        "the reproduction is on the alternate screen"
    );
    assert!(
        reproduced
            .screen_text()
            .expect("screen text")
            .contains("alternate content"),
        "the alternate content shows"
    );
    reproduced.feed(b"\x1b[?1049l");
    assert!(
        reproduced
            .screen_text()
            .expect("screen text")
            .contains("primary content"),
        "leaving the alternate screen reveals the primary"
    );

    state.leaving_alternate();
    mirror.feed(b"\x1b[?1049lback on primary");
    let primary_only = state
        .serialize(&mirror, Sequence(0))
        .expect("a serialization");
    let mut fresh = Vt::new(primary_only.columns, primary_only.rows).expect("a fresh oracle");
    fresh.feed(&primary_only.bytes);
    assert!(
        !fresh.in_alternate_screen().expect("a screen"),
        "a fresh serialization holds only the primary"
    );
}

/// A screen whose whole serialization exceeds the byte budget is brought within
/// it by dropping the oldest scrollback rows: the output stays under the bound,
/// `dropped_rows` is positive, the newest row still reproduces, and the oldest is
/// gone. Per-cell 24-bit color makes each row heavy enough to force the drop
/// through the mirror — its byte-budgeted scrollback cannot overflow the bound
/// with light rows, so this is the drop path's exercise short of the emulator.
///
/// # Panics
///
/// When the output exceeds the bound, nothing was dropped, or the newest row is
/// lost or the oldest kept.
#[test]
fn screen_serializer_bounds_the_output() {
    // 240×100 keeps the visible rows well within the budget while three hundred
    // heavy rows overflow it, so the drop is always the oldest scrollback and
    // never the visible; every row is tagged `RNNNN` to find the oldest and newest.
    let mut mirror = Mirror::new(240, 100).expect("a mirror");
    for row in 0..300_u32 {
        let mut line = format!("R{row:04}").into_bytes();
        for column in 0..235_u32 {
            let red = column % 256;
            let green = row % 256;
            let blue = column.wrapping_add(row) % 256;
            line.extend_from_slice(format!("\x1b[38;2;{red};{green};{blue}m.").as_bytes());
        }
        if row.saturating_add(1) < 300 {
            line.extend_from_slice(b"\r\n");
        }
        mirror.feed(&line);
    }
    let serialized = serialize(&mirror, Sequence(0)).expect("a serialization");
    assert!(
        serialized.bytes.len() <= MAXIMUM_SCREEN_BYTES,
        "within the bound: {} <= {MAXIMUM_SCREEN_BYTES}",
        serialized.bytes.len()
    );
    assert!(
        serialized.dropped_rows > 0,
        "the whole exceeded the bound, so the oldest rows were dropped: {}",
        serialized.dropped_rows
    );
    assert!(
        !serialized.bytes.windows(5).any(|window| window == b"R0000"),
        "the oldest row was dropped to fit"
    );

    let mut reproduced = Vt::new(serialized.columns, serialized.rows).expect("a fresh oracle");
    reproduced.feed(&serialized.bytes);
    assert!(
        reproduced
            .screen_text()
            .expect("screen text")
            .contains("R0299"),
        "the newest row reproduces"
    );
}
