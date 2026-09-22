//! Geometric symbols stay on their terminal columns.

mod support;

use iznik_app::grid::draw_list;
use iznik_app::vt::{VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;
use support::{key, open, snapshot};

/// A scanner spinner's squares each occupy one column instead of one shared run.
///
/// # Panics
/// Fails if same-colored symbol cells are merged, which is what piles their glyphs together.
#[test]
fn grid_keeps_symbol_glyph_on_its_cell() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 12, 3).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(0),
            bytes: "\u{2B1D}\u{2B1D}\u{25A0}\u{2B1D}X".as_bytes().to_vec(),
        })
        .expect("feed");
    let current = snapshot(&thread).expect("snapshot");
    let drawings = draw_list(&current, None).expect("draw list");
    let row = drawings.first().expect("row");
    let column_text: Vec<(u16, &str)> = row
        .runs
        .iter()
        .take(4)
        .map(|run| (run.column, run.text.as_str()))
        .collect();
    assert_eq!(
        column_text,
        vec![
            (0, "\u{2B1D}"),
            (1, "\u{2B1D}"),
            (2, "\u{25A0}"),
            (3, "\u{2B1D}"),
        ],
        "each spinner square keeps its column"
    );
    let label = row
        .runs
        .iter()
        .find(|run| run.text.starts_with('X'))
        .expect("label");
    assert_eq!(
        label.column, 4,
        "text after the spinner stays on its column"
    );
}
