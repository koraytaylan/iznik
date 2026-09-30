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
    assert_eq!(iznik_app::upload::byte_text(1536), "1.5 KiB");
    assert_eq!(iznik_app::upload::byte_text(2048), "2 KiB");
    assert_eq!(iznik_app::upload::byte_text(5 * 1024 * 1024), "5 MiB");
    assert_eq!(iznik_app::upload::percent(0, 0), 100);
    assert_eq!(iznik_app::upload::percent(1, 4), 25);
    assert_eq!(iznik_app::upload::percent(4, 4), 100);
    assert_eq!(iznik_app::upload::percent(1, 10_000), 0);
    assert_eq!(iznik_app::upload_rate::percent_points(1, 10_000), 1);
}

/// A pasted directory is one entry, and a file pasted beside it is another.
///
/// # Panics
///
/// When the grouping, the count, or the byte line differs.
#[test]
fn upload_groups_a_directory() {
    use iznik_app::upload::{groups, label, place_line, status_line};
    use iznik_app::upload_log::UploadPhase;
    let mut directory = sample("notes/", UploadPhase::Finished, 0, 0, true, true);
    directory.remote = Some("/home/me/notes".to_owned());
    let records = vec![
        directory,
        sample(
            "notes/a.txt",
            UploadPhase::Finished,
            2048,
            2048,
            false,
            false,
        ),
        sample("notes/sub/", UploadPhase::Finished, 0, 0, true, false),
        sample(
            "notes/sub/b.txt",
            UploadPhase::Sending,
            512,
            4096,
            false,
            false,
        ),
        sample("readme.txt", UploadPhase::Finished, 1024, 1024, false, true),
    ];
    let found = groups(&records);
    let tree = found.get(1).expect("the directory is the older paste");
    let file = found.first().expect("the newer file is first");
    assert_eq!(found.len(), 2);
    assert_eq!(tree.records.len(), 4);
    assert_eq!(label(tree), "notes");
    assert_eq!(status_line(tree), "1 of 2 finished \u{b7} 2.5 KiB / 6 KiB");
    assert_eq!(place_line(tree), "build \u{b7} /home/me/notes");
    assert_eq!(label(file), "readme.txt");
    assert_eq!(status_line(file), "finished \u{b7} 1 KiB");
}

/// Clearing the list drops a finished paste and keeps one that is still sending.
///
/// # Panics
///
/// When a busy paste is dropped, or a finished one is kept.
#[test]
fn upload_clear_leaves_a_busy_paste() {
    use iznik_app::upload::UploadPhase;
    use iznik_app::upload_list::{fail_open, keeping_busy};
    use iznik_protocol::identity::PaneId;

    let records = vec![
        sample("notes/", UploadPhase::Finished, 0, 0, true, true),
        sample("notes/a.txt", UploadPhase::Finished, 10, 10, false, false),
        sample("notes/b.txt", UploadPhase::Sending, 4, 10, false, false),
        sample("readme.txt", UploadPhase::Finished, 8, 8, false, true),
    ];
    let kept = keeping_busy(&records);
    assert_eq!(kept.len(), 3, "the directory paste stays together");
    assert!(
        kept.iter().all(|record| record.name.starts_with("notes")),
        "the finished file pasted beside it is gone"
    );
    let mut open = records;
    let names = fail_open(&mut open, "build", PaneId(1), "notes/");
    assert_eq!(names, vec!["notes/b.txt".to_owned()]);
    let sending = open
        .iter()
        .find(|record| record.name == "notes/b.txt")
        .expect("the file");
    assert_eq!(sending.phase, UploadPhase::Failed);
    assert_eq!(sending.detail.as_deref(), Some("cancelled"));
    assert!(
        open.iter()
            .any(|record| record.name == "notes/a.txt" && record.phase == UploadPhase::Finished),
        "a file already written stays written"
    );
}

/// A paste names bytes per second and the time left from two samples of its total.
///
/// # Panics
///
/// When the rate line differs, or a single sample produces one.
#[test]
fn upload_rate_reads_bytes_per_second_and_time_left() {
    use std::time::{Duration, Instant};

    use iznik_app::upload_log::UploadPhase;
    use iznik_app::upload_rate::{line, note};
    use iznik_protocol::identity::PaneId;

    let mut records = vec![
        sample("notes/", UploadPhase::Finished, 0, 0, true, true),
        sample("notes/a.txt", UploadPhase::Sending, 0, 4_096, false, false),
    ];
    let start = Instant::now();
    let later = start
        .checked_add(Duration::from_secs(2))
        .expect("two seconds fit");
    let mut rate = Vec::new();
    note(&mut rate, &records, "build", "notes/a.txt", start);
    assert!(
        line(&rate, "build", PaneId(1), "notes/", 4_096).is_none(),
        "one sample has no rate"
    );
    let file = records.get_mut(1).expect("the file");
    file.sent = 2_048;
    note(&mut rate, &records, "build", "notes/a.txt", later);
    assert_eq!(
        line(&rate, "build", PaneId(1), "notes/", 4_096),
        Some("1 KiB/s \u{b7} 2 seconds left".to_owned()),
        "the directory is rated by the bytes its files have sent"
    );
}

/// One record aimed at the same host and pane.
fn sample(
    name: &str,
    phase: iznik_app::upload_log::UploadPhase,
    sent: u64,
    total: u64,
    directory: bool,
    type_path: bool,
) -> iznik_app::upload_log::UploadRecord {
    iznik_app::upload_log::UploadRecord {
        name: name.to_owned(),
        host: "build".to_owned(),
        pane: iznik_protocol::identity::PaneId(1),
        local: PathBuf::from(name),
        sent,
        total,
        remote: None,
        detail: None,
        phase,
        type_path,
        directory,
        retry_once: false,
    }
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
