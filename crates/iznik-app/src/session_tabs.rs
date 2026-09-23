//! The tab each session was left on, written so the next launch opens it.
//!
//! The record is one line per fact: `shown <session> <tab> <host>` for the
//! tab a session was left on, and `open <session> <tab> <host>` for the tab
//! that was on screen. The host is the rest of the line, so a name may
//! contain spaces. A line that is not one of those two, or whose host holds
//! a control character, is skipped; the lines that parse still apply. The
//! file is replaced atomically, so a launch never reads a half-written
//! record.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use iznik_client::host::identity::HostId;
use iznik_protocol::identity::{SessionId, TabId};

use crate::window::{SessionKey, TabKey};

/// The file name of the record.
const SELECTION_FILE: &str = "session-tabs";

/// The word that marks the tab that was on screen.
const OPEN_MARK: &str = "open";

/// The word that marks the tab a session was left on.
const SHOWN_MARK: &str = "shown";

/// How many fields a line carries: the mark, the session, the tab, and the
/// host. The host is the last field and keeps every space it contains.
const RECORD_FIELDS: usize = 4;

/// The tabs a window left behind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTabs {
    /// The tab that was on screen, when one was.
    pub open: Option<TabKey>,
    /// The tab last shown in each session.
    pub shown: BTreeMap<SessionKey, TabId>,
}

/// The record this machine keeps: `$XDG_CONFIG_HOME/iznik/session-tabs`, or
/// `~/.config/iznik/session-tabs` when that variable is unset, or
/// `%APPDATA%\iznik\session-tabs` on Windows when neither home is set.
///
/// `None` when neither home is known, which is when there is nowhere to write.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    crate::configuration_file::default_path(SELECTION_FILE)
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
    crate::configuration_file::replace(path, encode(tabs).as_bytes())
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
