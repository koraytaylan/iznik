//! A stop is remembered for the file it names.

use iznik_client::host::manager::{forget_stop, remember_stop, stop_remembered};
use iznik_protocol::identity::PaneId;

/// One file can be stopped without stopping another.
///
/// # Panics
///
/// When the stop is missing, applies to a different file, or stays after it was seen.
#[test]
fn upload_stop_is_remembered_for_that_file() {
    let mut stops = Vec::new();
    let pane = PaneId(1);
    remember_stop(&mut stops, "build", pane, "notes/a.txt");
    remember_stop(&mut stops, "build", pane, "notes/a.txt");
    assert_eq!(stops.len(), 1, "asking twice records one stop");
    assert!(stop_remembered(&stops, "build", pane, "notes/a.txt"));
    assert!(!stop_remembered(&stops, "build", pane, "notes/b.txt"));
    assert!(!stop_remembered(&stops, "other", pane, "notes/a.txt"));
    assert!(forget_stop(&mut stops, "build", pane, "notes/a.txt"));
    assert!(!stop_remembered(&stops, "build", pane, "notes/a.txt"));
    assert!(!forget_stop(&mut stops, "build", pane, "notes/a.txt"));
}
