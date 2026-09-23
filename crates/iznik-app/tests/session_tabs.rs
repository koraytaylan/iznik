//! The selection record: a round trip, a line that does not parse, and a
//! missing file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use iznik_app::session_tabs::{self, SessionTabs};
use iznik_app::window::{SessionKey, TabKey};
use iznik_client::host::identity::HostId;
use iznik_protocol::identity::{SessionId, TabId};

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// A host whose name contains a space survives, and a bad line does not
/// drop the lines that parse.
///
/// # Panics
/// Panics when the decoded record differs.
#[test]
fn a_record_round_trips_and_skips_a_bad_line() {
    let mut shown = BTreeMap::new();
    shown.insert(session("build", 2), TabId(5));
    shown.insert(session("my host", 3), TabId(8));
    let tabs = SessionTabs {
        open: Some(key("my host", 3, 8)),
        shown,
    };
    let text = session_tabs::encode(&tabs);
    let extra = format!("{text}not a line\nopen 1 no tab\n");
    assert_eq!(
        session_tabs::decode(&extra),
        tabs,
        "a line that does not parse is skipped"
    );
}

/// A file that is not there loads as nothing, and a write creates its directory.
///
/// # Errors
///
/// Returns the operating system's error when the scratch directory or the
/// record cannot be written.
///
/// # Panics
/// Panics when the loaded record differs.
#[test]
fn a_missing_file_is_an_empty_record() -> Result<(), Failed> {
    let directory = scratch("missing")?;
    let loaded = session_tabs::load(&directory.0.join("session-tabs"));
    assert_eq!(loaded, SessionTabs::default(), "a missing file loads empty");
    let path = directory.0.join("nested").join("session-tabs");
    session_tabs::write(
        &path,
        &SessionTabs {
            open: Some(key("build", 2, 5)),
            shown: BTreeMap::from([(session("build", 2), TabId(5))]),
        },
    )?;
    let written = session_tabs::load(&path);
    assert_eq!(
        written.open,
        Some(key("build", 2, 5)),
        "the written tab on screen is read back"
    );
    assert_eq!(
        written.shown.get(&session("build", 2)),
        Some(&TabId(5)),
        "the written session tab is read back"
    );
    Ok(())
}

/// A scratch directory removed when the test ends.
///
/// # Errors
///
/// Returns the operating system's error when the directory cannot be created.
fn scratch(name: &str) -> Result<Scratch, Failed> {
    let path =
        std::env::temp_dir().join(format!("iznik-session-tabs-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(Scratch(path))
}

/// Removes its directory on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// A session key in the fixture.
fn session(host: &str, session: u64) -> SessionKey {
    SessionKey {
        host: HostId(host.to_owned()),
        session: SessionId(session),
    }
}

/// A tab key in the fixture.
fn key(host: &str, session: u64, tab: u64) -> TabKey {
    TabKey {
        host: HostId(host.to_owned()),
        session: SessionId(session),
        tab: TabId(tab),
    }
}
