//! Which OSC 8 targets a Command-click may hand to the platform.
//!
//! A program names a link's target, and the person sees only its label, so
//! the target is checked before the platform opens it: web and mail
//! addresses, and local files that are not programs. A `file:` target on
//! another host, a program, a script or an application bundle stays text,
//! because the platform would run it rather than show it.

use std::path::{Path, PathBuf};

/// Web schemes a Command-click may hand to the platform browser.
const WEB_SCHEMES: &[&str] = &["https", "http"];
/// The mail scheme, whose target needs an address rather than a hierarchy.
const MAIL_SCHEME: &str = "mailto";
/// The local-file scheme.
const FILE_SCHEME: &str = "file";
/// The one host name a `file:` target may name besides none.
const LOCAL_HOST: &str = "localhost";
/// The start of a hierarchical part.
const HIERARCHY: &str = "//";
/// Extensions the platform runs, or opens as an application, rather than shows.
const RUN_EXTENSIONS: &[&str] = &[
    "app", "exe", "com", "bat", "cmd", "msi", "ps1", "vbs", "scr", "lnk", "command", "tool", "sh",
    "jar", "pkg",
];
/// Permission bits that let any user run a file.
#[cfg(unix)]
const RUN_PERMISSIONS: u32 = 0o111;
/// The radix of a percent-encoded byte.
const HEXADECIMAL: u32 = 16;
/// Hex digits after `%` in one percent-encoded byte.
const ENCODED_DIGITS: usize = 2;

/// Whether `link` is an OSC 8 target safe to hand to the platform.
#[must_use]
pub fn safe_to_open(link: &str) -> bool {
    let Some((scheme, rest)) = link.split_once(':') else {
        return false;
    };
    if link
        .chars()
        .any(|character| character.is_ascii_control() || character.is_ascii_whitespace())
    {
        return false;
    }
    if scheme.eq_ignore_ascii_case(MAIL_SCHEME) {
        return !rest.is_empty();
    }
    let Some(body) = rest.strip_prefix(HIERARCHY).filter(|body| !body.is_empty()) else {
        return false;
    };
    if WEB_SCHEMES
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    {
        return true;
    }
    scheme.eq_ignore_ascii_case(FILE_SCHEME) && local_file(body).is_some_and(|path| shown(&path))
}

/// The local path of a `file:` target's hierarchical part, or `None` when it
/// names another host or cannot be decoded.
fn local_file(body: &str) -> Option<PathBuf> {
    let split = body.find('/').unwrap_or(body.len());
    let (host, path) = body.split_at_checked(split)?;
    if !host.is_empty() && !host.eq_ignore_ascii_case(LOCAL_HOST) {
        return None;
    }
    let path = path.split(['?', '#']).next()?;
    String::from_utf8(percent_decoded(path)?)
        .ok()
        .filter(|decoded| !decoded.is_empty())
        .map(PathBuf::from)
}

/// `text` with every `%XX` replaced by its byte; `None` for a malformed escape.
fn percent_decoded(text: &str) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let digits: Vec<u8> = bytes.by_ref().take(ENCODED_DIGITS).collect();
        let digits = std::str::from_utf8(&digits).ok()?;
        if digits.len() != ENCODED_DIGITS {
            return None;
        }
        decoded.push(u8::from_str_radix(digits, HEXADECIMAL).ok()?);
    }
    Some(decoded)
}

/// Whether an existing local file would be shown rather than run: not an
/// application bundle, not a program or script by extension, and not a file
/// with a permission to run it.
fn shown(path: &Path) -> bool {
    let runs = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            RUN_EXTENSIONS
                .iter()
                .any(|run| extension.eq_ignore_ascii_case(run))
        });
    if runs {
        return false;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_dir() || !runnable(&metadata)
}

/// Whether the file's permissions let someone run it.
#[cfg(unix)]
fn runnable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.is_file() && metadata.permissions().mode() & RUN_PERMISSIONS != 0
}

/// Whether the file's permissions let someone run it. Windows decides by
/// extension, which [`shown`] has already checked.
#[cfg(not(unix))]
fn runnable(_metadata: &std::fs::Metadata) -> bool {
    false
}
