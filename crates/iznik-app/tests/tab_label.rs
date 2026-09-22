//! A tab that has not been renamed shows the program in its focused pane, or
//! the directory when that program is a shell.

use iznik_app::tab_label::{is_default_tab_name, label_of, shown_tab_name};
use iznik_protocol::identity::{PaneId, TabId};
use iznik_protocol::model::{LayoutNode, Pane, Tab};

/// Fixture width, in cells.
const COLUMNS: u16 = 80;

/// Fixture height, in cells.
const ROWS: u16 = 24;

/// A pane with a title and a directory.
fn pane(id: u64, title: &str, directory: Option<&str>) -> Pane {
    Pane {
        id: PaneId(id),
        title: title.to_owned(),
        working_directory: directory.map(str::to_owned),
        columns: COLUMNS,
        rows: ROWS,
    }
}

/// Default tabs show the program or the directory; a renamed tab keeps its name.
///
/// # Panics
///
/// When a label disagrees.
#[test]
fn default_tabs_show_the_program_or_the_directory() {
    assert!(is_default_tab_name("shell"));
    assert!(is_default_tab_name("shell 2"));
    assert!(!is_default_tab_name("shell 1"));
    assert!(!is_default_tab_name("notes"));
    assert_eq!(label_of("shell", "vim", Some("/src/iznik")), "vim");
    assert_eq!(label_of("shell", "zsh", Some("/src/iznik")), "iznik");
    assert_eq!(label_of("shell", "", Some("/src/iznik")), "iznik");
    assert_eq!(label_of("shell", "", None), "shell");
    assert_eq!(label_of("notes", "vim", Some("/src/iznik")), "notes");

    let tab = Tab {
        id: TabId(1),
        name: "shell".to_owned(),
        panes: vec![
            pane(1, "", Some("/src/one")),
            pane(2, "btop", Some("/src/two")),
        ],
        layout: LayoutNode::Leaf(PaneId(1)),
    };
    assert_eq!(shown_tab_name(&tab, None), "one");
    assert_eq!(shown_tab_name(&tab, Some(PaneId(2))), "btop");
}
