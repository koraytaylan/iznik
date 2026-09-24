//! Which OSC 8 targets a Command-click hands to the platform.

use std::fs;

use iznik_app::grid::link::safe_to_open;

/// Web and mail addresses open; scripts and malformed targets do not.
///
/// # Panics
/// Fails when a target is opened or refused against the rule.
#[test]
fn web_and_mail_open_and_scripts_do_not() {
    assert!(safe_to_open("https://example.com/path"));
    assert!(safe_to_open("http://example.com"));
    assert!(safe_to_open("mailto:someone@example.com"));
    assert!(!safe_to_open("javascript:alert(1)"));
    assert!(!safe_to_open("https://"));
    assert!(!safe_to_open("https://example.com/a b"));
}

/// A `file:` target on another host is refused, whatever the file.
///
/// # Panics
/// Fails when a remote file target is opened.
#[test]
fn file_on_another_host_is_refused() {
    assert!(!safe_to_open("file://fileserver/share/notes.txt"));
    assert!(!safe_to_open("file://attacker.example/payload"));
}

/// A local document opens; a program, a script and an application bundle do not.
///
/// # Panics
/// Fails when a local program is opened or a document is refused.
#[test]
fn local_documents_open_and_programs_do_not() {
    let directory = std::env::temp_dir().join(format!("iznik-link-{}", std::process::id()));
    let _stale = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("directory");
    let document = directory.join("notes one.txt");
    fs::write(&document, "text").expect("document");
    let encoded = format!("file://{}", document.display()).replace(' ', "%20");
    assert!(safe_to_open(&encoded), "a document opens: {encoded}");
    let local = format!("file://localhost{}", document.display()).replace(' ', "%20");
    assert!(safe_to_open(&local), "localhost is this machine");
    let script = directory.join("run.sh");
    fs::write(&script, "#!/bin/sh\n").expect("script");
    assert!(
        !safe_to_open(&format!("file://{}", script.display())),
        "script"
    );
    let bundle = directory.join("Tool.app");
    fs::create_dir_all(&bundle).expect("bundle");
    assert!(
        !safe_to_open(&format!("file://{}", bundle.display())),
        "bundle"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let program = directory.join("program");
        fs::write(&program, "binary").expect("program");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("mode");
        assert!(
            !safe_to_open(&format!("file://{}", program.display())),
            "program"
        );
    }
    assert!(
        !safe_to_open(&format!("file://{}/missing", directory.display())),
        "a file that is not there is not opened"
    );
    let _removed = fs::remove_dir_all(&directory);
}

/// Only a plain file whose extension names a document opens: files the
/// platform runs that no list of programs names, directories, bundles, and
/// links to elsewhere stay text.
///
/// # Panics
/// Fails when anything but a plain document is opened.
#[test]
fn only_plain_documents_open() {
    let directory = std::env::temp_dir().join(format!("iznik-link-kinds-{}", std::process::id()));
    let _stale = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("directory");
    let target = |name: &str| format!("file://{}", directory.join(name).display());
    for name in [
        "Shell.terminal",
        "place.fileloc",
        "place.inetloc",
        "page.webloc",
        "script.scpt",
        "script.applescript",
        "page.html",
        "no-extension",
    ] {
        fs::write(directory.join(name), "content").expect("file");
        assert!(!safe_to_open(&target(name)), "{name} is not a document");
    }
    for name in ["Run.workflow", "Pane.prefPane", "Saver.saver", "folder"] {
        fs::create_dir_all(directory.join(name)).expect("bundle");
        assert!(!safe_to_open(&target(name)), "{name} is a directory");
    }
    fs::create_dir_all(directory.join("folder.txt")).expect("folder");
    assert!(
        !safe_to_open(&target("folder.txt")),
        "a directory named like a document"
    );
    for name in ["notes.md", "picture.PNG", "paper.pdf", "main.rs"] {
        fs::write(directory.join(name), "content").expect("document");
        assert!(safe_to_open(&target(name)), "{name} is a document");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(directory.join("Shell.terminal"), directory.join("link.txt"))
            .expect("symbolic link");
        assert!(
            !safe_to_open(&target("link.txt")),
            "a link named like a document"
        );
    }
    let _removed = fs::remove_dir_all(&directory);
}
