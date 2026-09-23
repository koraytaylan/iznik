//! Every glyph is painted at its own cell's column, whatever its font's advance.

mod support;

use gpui_kit::{FontId, GlyphId, ShapedGlyph, ShapedRun, point, px};
use iznik_app::grid::{align_glyphs, draw_list};
use iznik_app::vt::{VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;
use support::{key, open, snapshot};

/// The width of one terminal column in the fixture, in logical pixels.
const CELL_WIDTH: f32 = 8.0;

/// One shaped glyph for the byte at `index`, where shaping put it.
fn glyph(index: usize, left: f32) -> ShapedGlyph {
    ShapedGlyph {
        id: GlyphId(1),
        position: point(px(left), px(0.0)),
        index,
        is_emoji: false,
    }
}

/// The horizontal positions of every glyph, in order.
fn positions(runs: &[ShapedRun]) -> Vec<f32> {
    runs.iter()
        .flat_map(|run| &run.glyphs)
        .map(|glyph| f32::from(glyph.position.x))
        .collect()
}

/// A narrow fallback glyph no longer pulls the text after it off its column.
///
/// # Panics
/// Fails when a glyph is not at its cell's column.
#[test]
fn fallback_advance_does_not_move_later_cells() {
    let mut runs = vec![
        ShapedRun {
            font_id: FontId(0),
            glyphs: vec![glyph(0, 0.0)],
        },
        ShapedRun {
            font_id: FontId(1),
            glyphs: vec![glyph(1, 8.0), glyph(4, 13.0)],
        },
    ];
    align_glyphs(&mut runs, &[0, 1, 4], px(CELL_WIDTH));
    assert_eq!(positions(&runs), [0.0, 8.0, 16.0], "one glyph per column");
}

/// A combining mark keeps its place over its base; the next cell is on its column.
///
/// # Panics
/// Fails when the mark moves away from its base or the next cell drifts.
#[test]
fn combining_marks_keep_their_offset_from_the_base() {
    let mut runs = vec![ShapedRun {
        font_id: FontId(0),
        glyphs: vec![glyph(0, 0.0), glyph(1, 1.5), glyph(3, 6.0)],
    }];
    align_glyphs(&mut runs, &[0, 3], px(CELL_WIDTH));
    assert_eq!(positions(&runs), [0.0, 1.5, 8.0], "mark stays on its base");
}

/// A run records where each of its cells' text begins.
///
/// # Panics
/// Fails when a cell's offset is missing or wrong.
#[test]
fn runs_record_where_each_cell_begins() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 4, 1).expect("open");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(0),
            bytes: "a\u{e9}b".as_bytes().to_vec(),
            receipt: None,
        })
        .expect("feed");
    let frame = snapshot(&thread).expect("snapshot");
    let rows = draw_list(&frame, None).expect("draw list");
    let run = &rows[0].runs[0];
    assert!(run.text.starts_with("a\u{e9}b"), "one run: {:?}", run.text);
    assert_eq!(run.starts[..3], [0, 1, 3], "the accented cell is two bytes");
}
