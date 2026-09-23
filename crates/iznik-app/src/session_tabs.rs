//! The tab each session was left on, written so the next launch opens it.
//!
//! The record is one line per fact: `shown <session> <tab> <host>` for the
//! tab a session was left on, and `open <session> <tab> <host>` for the tab
//! that was on screen. The host is the rest of the line, so a name may
//! contain spaces. A line that is not one of those two, or whose host holds
//! a control character, is skipped; the lines that parse still apply. The
//! file is replaced by renaming a temporary beside it, so a launch never
//! reads a half-written record.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use iznik_client::host::identity::HostId;
use iznik_protocol::identity::{SessionId, TabId};

use crate::window::{SessionKey, TabKey};

/// The directory name under `$HOME` when `XDG_CONFIG_HOME` is unset.
const CONFIGURATION_DIRECTORY: &str = ".config";

/// The directory under the configuration home that holds this record.
const APPLICATION_DIRECTORY: &str = "iznik";

/// The file name of the record.
const SELECTION_FILE: &str = "session-tabs";

/// The environment variable that names the configuration home.
const CONFIGURATION_HOME: &str = "XDG_CONFIG_HOME";

/// The environment variable that names the home directory.
const HOME: &str = "HOME";

/// The word that marks the tab that was on screen.
const OPEN_MARK: &str = "open";

/// The word that marks the tab a session was left on.
const SHOWN_MARK: &str = "shown";

/// How many fields a line carries: the mark, the session, the tab, and the
/// host. The host is the last field and keeps every space it contains.
const RECORD_FIELDS: usize = 4;

/// The extension of the temporary file a record is written to before it
/// replaces the one a launch reads.
const TEMPORARY_EXTENSION: &str = "temporary";

/// The tabs a window left behind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTabs {
    /// The tab that was on screen, when one was.
    pub open: Option<TabKey>,
    /// The tab last shown in each session.
    pub shown: BTreeMap<SessionKey, TabId>,
}

/// The record this machine keeps: `$XDG_CONFIG_HOME/iznik/session-tabs`, or
/// `~/.config/iznik/session-tabs` when that variable is unset.
///
/// `None` when neither home is known, which is when there is nowhere to write.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    let directory = std::env::var_os(CONFIGURATION_HOME)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(HOME)
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(CONFIGURATION_DIRECTORY))
        })?;
    Some(directory.join(APPLICATION_DIRECTORY).join(SELECTION_FILE))
}

/// Read a record. A missing or unreadable file is an empty record: the window
/// still opens, on the first tab, which is what it did before a record existed.
#[must_use]
pub fn load(path: &Path) -> SessionTabs {
    std::fs::read_to_string(path).map_or_else(|_| SessionTabs::default(), |text| decode(&text))
}

/// Replace the record at `path`, creating its directory when it is missing.
///
/// # Errors
///
/// Returns the operating system's error when the directory cannot be created
/// or the file cannot be replaced. The previous record is left in place when
/// the temporary file cannot be written.
pub fn write(path: &Path, tabs: &SessionTabs) -> Result<(), std::io::Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(TEMPORARY_EXTENSION);
    std::fs::write(&temporary, encode(tabs))?;
    replace(&temporary, path)
}

/// The record as the file holds it.
#[must_use]
pub fn encode(tabs: &SessionTabs) -> String {
    let mut text = String::new();
    for (key, tab) in &tabs.shown {
        push_line(&mut text, SHOWN_MARK, &key.host, key.session, *tab);
    }
    if let Some(open) = &tabs.open {
        push_line(&mut text, OPEN_MARK, &open.host, open.session, open.tab);
    }
    text
}

/// The record a file's text holds. A line that does not parse is skipped.
#[must_use]
pub fn decode(text: &str) -> SessionTabs {
    let mut tabs = SessionTabs::default();
    for line in text.lines() {
        let Some((open_line, key)) = parse_line(line) else {
            continue;
        };
        tabs.shown.insert(session_of(&key), key.tab);
        if open_line {
            tabs.open = Some(key);
        }
    }
    tabs
}

/// Append one line when the host can sit on it.
fn push_line(text: &mut String, mark: &str, host: &HostId, session: SessionId, tab: TabId) {
    if host_fits(&host.0) {
        let _written = writeln!(text, "{mark} {} {} {}", session.0, tab.0, host.0);
    }
}

/// One parsed line: whether it names the tab on screen, and that tab.
fn parse_line(line: &str) -> Option<(bool, TabKey)> {
    let mut parts = line.splitn(RECORD_FIELDS, ' ');
    let mark = parts.next()?.trim();
    let session = parse_id(parts.next()?)?;
    let tab = parse_id(parts.next()?)?;
    let host = parts.next()?.trim();
    if !host_fits(host) {
        return None;
    }
    let open_line = match mark {
        OPEN_MARK => true,
        SHOWN_MARK => false,
        _ => return None,
    };
    Some((
        open_line,
        TabKey {
            host: HostId(host.to_owned()),
            session: SessionId(session),
            tab: TabId(tab),
        },
    ))
}

/// A decimal id, or nothing when the field is not one.
fn parse_id(text: &str) -> Option<u64> {
    text.parse().ok()
}

/// Whether a host can occupy the last field of one line.
fn host_fits(host: &str) -> bool {
    !host.is_empty() && !host.chars().any(char::is_control)
}

/// The session a tab belongs to.
fn session_of(key: &TabKey) -> SessionKey {
    SessionKey {
        host: key.host.clone(),
        session: key.session,
    }
}

/// Move `temporary` onto `path`. A rename that cannot replace an existing
/// file removes that file and tries once more.
///
/// # Errors
///
/// Returns the operating system's error when the record cannot be replaced.
fn replace(temporary: &Path, path: &Path) -> Result<(), std::io::Error> {
    if std::fs::rename(temporary, path).is_ok() {
        return Ok(());
    }
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temporary, path)
}
