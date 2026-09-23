//! Box drawing, block elements and powerline separators are drawn to the cell.

mod support;

use gpui_kit::{Bounds, point, px, size};
use iznik_app::grid::{CellShape, cell_shapes, draw_list};
use iznik_app::vt::{VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;
use support::{key, open, snapshot};

/// A cell eight pixels wide and sixteen tall, at the window origin.
fn cell() -> Bounds<gpui_kit::Pixels> {
    Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(16.0)))
}

/// The filled rectangles of `character`, as left, top, right, bottom and
/// opacity, in order; none for a character that is not drawn.
fn fills(character: char) -> Vec<(f32, f32, f32, f32, f32)> {
    let mut found: Vec<_> = cell_shapes(character, cell())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|shape| match shape {
            CellShape::Fill { bounds, opacity } => Some((
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.right()),
                f32::from(bounds.bottom()),
                opacity,
            )),
            _ => None,
        })
        .collect();
    found.sort_by(|left, right| {
        left.partial_cmp(right)
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    found
}

/// A horizontal line spans the whole cell width, so neighbours join.
///
/// # Panics
/// Fails when the line leaves a gap at either edge.
#[test]
fn horizontal_line_spans_the_cell() {
    assert_eq!(
        fills('\u{2500}'),
        [(0.0, 7.0, 4.0, 8.0, 1.0), (3.0, 7.0, 8.0, 8.0, 1.0)],
        "one light line through the middle, edge to edge"
    );
}

/// A double corner closes its outer and its inner line.
///
/// # Panics
/// Fails when the double lines of a corner do not meet.
#[test]
fn double_corner_meets_itself() {
    assert_eq!(
        fills('\u{2554}'),
        [
            (2.0, 6.0, 3.0, 16.0, 1.0),
            (2.0, 6.0, 8.0, 7.0, 1.0),
            (4.0, 8.0, 5.0, 16.0, 1.0),
            (4.0, 8.0, 8.0, 9.0, 1.0),
        ],
        "outer lines meet at (2, 6), inner lines at (4, 8)"
    );
}

/// Block elements fill their eighths of the cell, and shades are translucent.
///
/// # Panics
/// Fails when a block covers the wrong part of the cell.
#[test]
fn blocks_fill_their_part_of_the_cell() {
    assert_eq!(
        fills('\u{2588}'),
        [(0.0, 0.0, 8.0, 16.0, 1.0)],
        "full block"
    );
    assert_eq!(
        fills('\u{2584}'),
        [(0.0, 8.0, 8.0, 16.0, 1.0)],
        "lower half"
    );
    assert_eq!(fills('\u{258C}'), [(0.0, 0.0, 4.0, 16.0, 1.0)], "left half");
    assert_eq!(
        fills('\u{2591}'),
        [(0.0, 0.0, 8.0, 16.0, 0.25)],
        "light shade"
    );
    assert_eq!(fills('\u{259A}').len(), 2, "two diagonal quadrants");
}

/// Powerline separators are a solid triangle and a thin chevron that touch
/// the cell's edges.
///
/// # Panics
/// Fails when a separator is not the expected shape.
#[test]
fn powerline_separators_touch_the_cell_edges() {
    let solid = cell_shapes('\u{E0B0}', cell()).expect("solid");
    assert_eq!(
        solid,
        [CellShape::Polygon(vec![
            point(px(0.0), px(0.0)),
            point(px(8.0), px(8.0)),
            point(px(0.0), px(16.0)),
        ])],
        "right-pointing triangle"
    );
    let thin = cell_shapes('\u{E0B3}', cell()).expect("thin");
    assert!(
        matches!(thin.as_slice(), [CellShape::Stroke { points, .. }] if points[1] == point(px(0.0), px(8.0))),
        "left-pointing chevron: {thin:?}"
    );
    assert!(
        cell_shapes('a', cell()).is_none(),
        "text is left to the font"
    );
}

/// A frame's characters share one run that the font does not shape.
///
/// # Panics
/// Fails when box drawing is shaped with the text beside it.
#[test]
fn box_drawing_runs_are_kept_from_text() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 6, 1).expect("open");
    thread
        .send(VtCommand::Feed {
            key: key(),
            sequence: Sequence(0),
            bytes: "a\u{2500}\u{253C}\u{2500}b".as_bytes().to_vec(),
            receipt: None,
        })
        .expect("feed");
    let frame = snapshot(&thread).expect("snapshot");
    let rows = draw_list(&frame, None).expect("draw list");
    let texts: Vec<&str> = rows[0].runs.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(texts[..3], ["a", "\u{2500}\u{253C}\u{2500}", "b"], "runs");
}
