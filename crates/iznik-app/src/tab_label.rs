//! The name a tab chip shows. A person who has renamed a tab keeps that name.
//! A tab still called `shell` — the name a new tab is given — shows the
//! foreground program, or the directory when the foreground program is the
//! shell or has not said what it is.

use iznik_protocol::identity::PaneId;
use iznik_protocol::model::Tab;
use iznik_protocol::program::{directory_label, program_label};

/// The name a new tab is given, matching the host's first tab.
pub const DEFAULT_TAB_NAME: &str = "shell";

/// The name to draw for `tab`, following `focus` when that pane is in the tab.
#[must_use]
pub fn shown_tab_name(tab: &Tab, focus: Option<PaneId>) -> String {
    let pane = focus
        .and_then(|focused| tab.panes.iter().find(|pane| pane.id == focused))
        .or_else(|| tab.panes.first());
    label_of(
        &tab.name,
        pane.map_or("", |pane| pane.title.as_str()),
        pane.and_then(|pane| pane.working_directory.as_deref()),
    )
}

/// Whether `name` is one a new tab is given: `shell`, or `shell` and a number
/// from two up, which is how further tabs are numbered.
#[must_use]
pub fn is_default_tab_name(name: &str) -> bool {
    if name == DEFAULT_TAB_NAME {
        return true;
    }
    let Some(rest) = name.strip_prefix(DEFAULT_TAB_NAME) else {
        return false;
    };
    let Some(number) = rest.strip_prefix(' ') else {
        return false;
    };
    number.parse::<u32>().is_ok_and(|value| value > 1)
}

/// The chip text for a tab named `name` whose focused pane has `title` and
/// `directory`.
#[must_use]
pub fn label_of(name: &str, title: &str, directory: Option<&str>) -> String {
    if !is_default_tab_name(name) {
        return name.to_owned();
    }
    if let Some(program) = program_label(title) {
        return program.to_owned();
    }
    if let Some(directory) = directory.and_then(directory_label) {
        return directory.to_owned();
    }
    name.to_owned()
}
