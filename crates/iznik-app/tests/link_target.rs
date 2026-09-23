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
