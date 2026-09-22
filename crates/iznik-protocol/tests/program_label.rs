//! A discovered foreground program becomes the pane title only when the title
//! does not already name something stronger, and a directory's label is its
//! last component.

use iznik_protocol::program::{
    directory_label, is_shell_program, next_title, program_file_name, program_label,
};

/// A running program fills an empty title, replaces a shell name or a path,
/// and leaves a title the program set itself.
///
/// # Panics
///
/// When a case disagrees.
#[test]
fn next_title_uses_the_running_program() {
    assert_eq!(next_title("", None, "sleep"), Some("sleep".to_owned()));
    assert_eq!(next_title("zsh", None, "sleep"), Some("sleep".to_owned()));
    assert_eq!(
        next_title("/tmp/src", None, "/bin/sleep"),
        Some("sleep".to_owned())
    );
    assert_eq!(
        next_title("README - vim", None, "sleep"),
        None,
        "a program's own title stays"
    );
    assert_eq!(
        next_title("sleep", Some("sleep"), "zsh"),
        Some(String::new()),
        "returning to the shell clears the title sampling wrote"
    );
    assert_eq!(next_title("README - vim", Some("sleep"), "zsh"), None);
    assert_eq!(next_title("", None, "zsh"), None);
}

/// A directory's label is its last component, and a shell's file name is a shell.
///
/// # Panics
///
/// When a label disagrees.
#[test]
fn directory_label_is_the_last_component() {
    assert_eq!(program_label("vim"), Some("vim"));
    assert_eq!(program_label("zsh"), None);
    assert_eq!(program_label("user@host: ~/src"), None);
    assert_eq!(directory_label("/Users/a/iznik"), Some("iznik"));
    assert_eq!(directory_label("/"), None);
    assert_eq!(program_file_name("/bin/zsh"), "zsh");
    assert!(is_shell_program("cmd.exe"));
    assert!(!is_shell_program("sleep"));
}
