//! Pasting a copied file offers the file, not the path text that rides with it,
//! and the path the host writes is quoted for a shell.

use std::path::PathBuf;

use gpui_kit::{ClipboardEntry, ClipboardItem, ExternalPaths};
use iznik_app::upload::{ClipboardPaste, paste_clipboard, shell_quote};

/// A copied file is offered as a file even when the clipboard also holds its path as text.
///
/// # Panics
///
/// When the clipboard is read as text, or the path differs.
#[test]
fn upload_takes_file_paths_before_the_path_text() {
    let mut external = ExternalPaths::default();
    external.0.push(PathBuf::from("/tmp/notes.txt"));
    let item = ClipboardItem {
        entries: vec![
            ClipboardEntry::ExternalPaths(external),
            ClipboardEntry::String(" /tmp/notes.txt".to_owned().into()),
        ],
    };
    let ClipboardPaste::Files(paths) = paste_clipboard(&item) else {
        panic!("the file was offered as text");
    };
    assert_eq!(paths, vec![PathBuf::from("/tmp/notes.txt")]);
}

/// A byte count reads in kibibytes and mebibytes, and progress is a percent.
///
/// # Panics
///
/// When a reading or a percent differs.
#[test]
fn upload_reads_any_length_and_its_progress() {
    assert_eq!(iznik_app::upload::byte_text(0), "0 B");
    assert_eq!(iznik_app::upload::byte_text(2048), "2 KiB");
    assert_eq!(iznik_app::upload::byte_text(5 * 1024 * 1024), "5 MiB");
    assert_eq!(iznik_app::upload::percent(0, 0), 100);
    assert_eq!(iznik_app::upload::percent(1, 4), 25);
    assert_eq!(iznik_app::upload::percent(4, 4), 100);
}

/// A directory is sent as itself and everything inside it, parents first.
///
/// # Panics
///
/// When the listing differs, or a link is offered as a file.
#[test]
fn upload_lists_a_directory_tree() {
    let root = std::env::temp_dir().join(format!("iznik-upload-tree-{}", std::process::id()));
    let _cleared = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).expect("the tree is created");
    std::fs::write(root.join("a.txt"), b"a").expect("a file is written");
    std::fs::write(root.join("sub").join("b.txt"), b"b").expect("a nested file is written");
    let listed =
        iznik_app::upload_tree::collect(std::slice::from_ref(&root)).expect("the tree is listed");
    let names: Vec<_> = listed.iter().map(|item| item.name.as_str()).collect();
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the directory has a name");
    assert_eq!(
        names,
        vec![
            format!("{root_name}/"),
            format!("{root_name}/a.txt"),
            format!("{root_name}/sub/"),
            format!("{root_name}/sub/b.txt"),
        ]
    );
    assert!(
        listed
            .first()
            .is_some_and(|item| item.type_path && item.directory)
    );
    assert!(
        listed
            .get(1)
            .is_some_and(|item| !item.type_path && !item.directory)
    );
    let _removed = std::fs::remove_dir_all(&root);
}

/// A record round-trips, and a refusal names the byte a resume continues from.
///
/// # Panics
///
/// When a field differs, or the byte is not read.
#[test]
fn upload_log_round_trip() {
    use iznik_app::upload_log::{UploadPhase, UploadRecord};
    use iznik_protocol::identity::PaneId;
    let record = UploadRecord {
        name: "notes/a.txt".to_owned(),
        host: "dev box".to_owned(),
        pane: PaneId(7),
        local: PathBuf::from("/tmp/notes/a.txt"),
        sent: 5,
        total: 11,
        remote: None,
        detail: Some("line\n".to_owned()),
        phase: UploadPhase::Sending,
        type_path: false,
        directory: false,
        retry_once: true,
    };
    let decoded = iznik_app::upload_log::decode(&iznik_app::upload_log::encode(
        std::slice::from_ref(&record),
    ));
    assert_eq!(decoded, vec![record]);
    assert_eq!(
        iznik_app::upload::written_at("\"notes.txt\" is at byte 5"),
        Some(5)
    );
    assert_eq!(
        iznik_app::upload::written_at("the host refused the file"),
        None
    );
}

/// A remote path is quoted so a shell reads it as one word.
///
/// # Panics
///
/// When a quoted path differs from the form a shell expects.
#[test]
fn upload_quotes_a_remote_path_for_the_shell() {
    assert_eq!(shell_quote("/home/me/notes.txt"), "/home/me/notes.txt ");
    assert_eq!(
        shell_quote("/home/me/my notes.txt"),
        "'/home/me/my notes.txt' "
    );
    assert_eq!(shell_quote("it's"), r"'it'\''s' ");
    assert_eq!(
        shell_quote("C:\\Users\\me\\notes.txt"),
        "\"C:\\Users\\me\\notes.txt\" "
    );
}
