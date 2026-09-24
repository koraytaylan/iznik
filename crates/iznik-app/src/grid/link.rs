//! Which OSC 8 targets a Command-click may hand to the platform.
//!
//! A program names a link's target, and the person sees only its label, so
//! the target is checked before the platform opens it: web and mail
//! addresses, and local documents. A local file opens only when its
//! extension names a kind of document the platform shows — text, an image,
//! a PDF, source code — and it is a plain file that nobody may run. Anything
//! else stays text: a `file:` target on another host, a directory or bundle,
//! a link to elsewhere, and every kind of file not known to be only shown,
//! because a list of what the platform runs is never complete (`.terminal`,
//! `.webloc`, `.workflow`, `.scpt` and many more).

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
/// Extensions of the documents a platform shows rather than runs: text,
/// images, PDFs, sound and video, and source code no platform runs by
/// opening it. Scripts a platform may run when opened (`.js`, `.py`, `.html`
/// and `.svg`, which carry script) are not here.
const SHOWN_EXTENSIONS: &[&str] = &[
    "txt", "text", "md", "markdown", "rst", "log", "csv", "tsv", "json", "yaml", "yml", "toml",
    "ini", "conf", "xml", "diff", "patch", "pdf", "png", "jpg", "jpeg", "gif", "bmp", "tif",
    "tiff", "webp", "heic", "mp3", "wav", "m4a", "mp4", "mov", "rs", "c", "h", "cc", "cpp", "hpp",
    "go", "java", "swift", "kt",
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

/// Whether an existing local file would be shown rather than run: its
/// extension names a document, and it is a plain file — not a directory or
/// bundle, not a symbolic link to something else — with no permission to run
/// it. The extension is checked first, so a target that is not a document
/// costs no look at the file system; the look that follows is one `lstat`.
fn shown(path: &Path) -> bool {
    let document = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SHOWN_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        });
    if !document {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    metadata.is_file() && !runnable(&metadata)
}

/// Whether the file's permissions let someone run it.
#[cfg(unix)]
fn runnable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & RUN_PERMISSIONS != 0
}

/// Whether the file's permissions let someone run it. Windows decides by
/// extension, which [`shown`] has already checked.
#[cfg(not(unix))]
fn runnable(_metadata: &std::fs::Metadata) -> bool {
    false
}
