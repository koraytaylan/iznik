//! What a delta is, for a log: its kind and the ids it names, and nothing a
//! person typed or a program printed.
//!
//! The log is bundled into a bug report by `iznik doctor`. A tab's name, a
//! pane's title and its working directory are the user's — a title is often a
//! command line, secrets and all — and a delta that carries them is logged as
//! the ids it concerns instead.

use iznik_protocol::delta::Delta;

/// The kind of `delta` and the ids it names.
pub(super) fn summary(delta: &Delta) -> String {
    match delta {
        Delta::SessionAdded { session } => format!("SessionAdded session={}", session.id.0),
        Delta::SessionRenamed { session, .. } => format!("SessionRenamed session={}", session.0),
        Delta::SessionRemoved { session } => format!("SessionRemoved session={}", session.0),
        Delta::TabAdded { session, tab, .. } => {
            format!("TabAdded session={} tab={}", session.0, tab.id.0)
        }
        Delta::TabRenamed { tab, .. } => format!("TabRenamed tab={}", tab.0),
        Delta::TabRemoved { tab } => format!("TabRemoved tab={}", tab.0),
        Delta::TabsReordered { session, .. } => format!("TabsReordered session={}", session.0),
        Delta::PaneAdded { tab, pane } => format!("PaneAdded tab={} pane={}", tab.0, pane.id.0),
        Delta::PaneRemoved { pane, .. } => format!("PaneRemoved pane={}", pane.0),
        Delta::PaneMoved { pane, to_tab } => format!("PaneMoved pane={} tab={}", pane.0, to_tab.0),
        Delta::LayoutChanged { tab, .. } => format!("LayoutChanged tab={}", tab.0),
        Delta::PaneTitle { pane, .. } => format!("PaneTitle pane={}", pane.0),
        Delta::PaneWorkingDirectory { pane, .. } => format!("PaneWorkingDirectory pane={}", pane.0),
        Delta::PaneResized { pane, .. } => format!("PaneResized pane={}", pane.0),
        Delta::SessionsReordered { .. } => "SessionsReordered".to_owned(),
    }
}
