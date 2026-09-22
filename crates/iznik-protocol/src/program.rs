//! What a pane is running, as a tab can show it: a program name, or the
//! directory when the foreground program is the shell itself.
//!
//! A shell's own name is a poor tab name — every tab would say `zsh` — and a
//! title that is a path, an address or a shell name is the directory's job.
//! The server writes a discovered program into the pane title only when the
//! title still says nothing stronger, and the client shows that title or the
//! directory's last component.

/// Program names that are shells. A foreground shell names the directory, not
/// itself. Compared without regard to case, after a Windows `.exe` ending.
const SHELL_PROGRAMS: &[&str] = &[
    "bash",
    "busybox",
    "cmd",
    "csh",
    "dash",
    "elvish",
    "fish",
    "ksh",
    "login",
    "nu",
    "powershell",
    "pwsh",
    "sh",
    "tcsh",
    "xonsh",
    "zsh",
];

/// The ending Windows adds to a program file, stripped before the name is
/// compared with [`SHELL_PROGRAMS`].
const WINDOWS_PROGRAM_ENDING: &str = ".exe";

/// The program's file name: the last path component, or the text itself when
/// it has none.
#[must_use]
pub fn program_file_name(name: &str) -> &str {
    let trimmed = name.trim();
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|component| !component.is_empty())
        .unwrap_or(trimmed)
}

/// Whether `name` is a shell, so a tab should show the directory instead.
#[must_use]
pub fn is_shell_program(name: &str) -> bool {
    let bare = without_windows_ending(program_file_name(name));
    SHELL_PROGRAMS
        .iter()
        .any(|shell| bare.eq_ignore_ascii_case(shell))
}

/// A title worth showing as the program: not empty, not a shell, and not a
/// location (a path, a `file` URL's host, or a `user@host` prompt).
#[must_use]
pub fn program_label(title: &str) -> Option<&str> {
    let title = title.trim();
    if title.is_empty() || title.contains(['/', '\\', '@', ':', '~']) || is_shell_program(title) {
        return None;
    }
    Some(title)
}

/// The last component of a directory path, for a tab named by where it is.
#[must_use]
pub fn directory_label(path: &str) -> Option<&str> {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|component| !component.is_empty())
}

/// The title to publish for a discovered foreground program.
///
/// `written` is the title this sampling last published, when it has. A title
/// the program or the shell set itself is left alone, unless it is empty, a
/// shell name or a location — those are replaced by the running program. A
/// return to the shell clears only a title sampling wrote.
///
/// Returns the new title when it differs from `current`.
#[must_use]
pub fn next_title(current: &str, written: Option<&str>, running: &str) -> Option<String> {
    let shown = if running.is_empty() || is_shell_program(running) {
        ""
    } else {
        program_file_name(running)
    };
    let shown = without_windows_ending(shown);
    if current == shown {
        return None;
    }
    let ours = written == Some(current);
    if shown.is_empty() {
        return ours.then(String::new);
    }
    (ours || program_label(current).is_none()).then(|| shown.to_owned())
}

/// `name` without a trailing Windows program ending.
fn without_windows_ending(name: &str) -> &str {
    let Some(end) = name.len().checked_sub(WINDOWS_PROGRAM_ENDING.len()) else {
        return name;
    };
    let ending = name.get(end..).unwrap_or("");
    if ending.eq_ignore_ascii_case(WINDOWS_PROGRAM_ENDING) {
        name.get(..end).unwrap_or(name)
    } else {
        name
    }
}
