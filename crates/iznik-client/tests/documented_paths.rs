//! What the README says iznik puts on a host, held to the code that puts it
//! there.
//!
//! A tool that installs binaries on other people's machines says so up front,
//! and a promise about somebody else's filesystem is worth exactly as much as
//! its being true. So every path that section names is looked for in the
//! source that writes it: a path added to the document with nothing behind it
//! fails here, and so does a constant renamed without the document following.

use std::path::{Path, PathBuf};

/// The section of the README this holds to the code.
const SECTION: &str = "## What iznik puts on a host";

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// One thing the section promises: how it is written there, and the text that
/// must be in the file that does it.
struct Promise {
    /// As the README writes it.
    documented: &'static str,
    /// Where the code that writes it lives, from the workspace root.
    source: &'static str,
    /// What must appear in that file.
    named: &'static str,
}

/// Every path the section names.
///
/// The three under the runtime directory are the daemon's own: it is the
/// server that makes them, and the client only takes them away again.
const PROMISES: &[Promise] = &[
    Promise {
        documented: "`<prefix>/bin/iznik-server`",
        source: "crates/iznik-client/src/bootstrap/upload.rs",
        named: "pub const BINARY_NAME: &str = \"iznik-server\";",
    },
    Promise {
        documented: "`<prefix>/bin/iznik-server`",
        source: "crates/iznik-client/src/bootstrap/upload.rs",
        named: "pub const BINARY_DIRECTORY: &str = \"bin\";",
    },
    Promise {
        documented: "`<prefix>/terminfo`",
        source: "crates/iznik-client/src/bootstrap/upload.rs",
        named: "pub const TERMINFO_DIRECTORY: &str = \"terminfo\";",
    },
    Promise {
        documented: "`$XDG_DATA_HOME/iznik`",
        source: "crates/iznik-client/src/bootstrap/probe.rs",
        named: "data=${XDG_DATA_HOME:-$home/.local/share}",
    },
    Promise {
        documented: "`$HOME/.local/share/iznik`",
        source: "crates/iznik-client/src/bootstrap/probe.rs",
        named: "\"$home/.local/share/iznik\"",
    },
    Promise {
        documented: "`$XDG_RUNTIME_DIR/iznik`",
        source: "crates/iznik-client/src/bootstrap/mod.rs",
        named: "runtime=\"$XDG_RUNTIME_DIR/iznik\"",
    },
    Promise {
        documented: "`$TMPDIR/iznik-<user_id>`",
        source: "crates/iznik-client/src/bootstrap/mod.rs",
        named: "runtime=\"${TMPDIR:-/tmp}/iznik-$(id -u)\"",
    },
    Promise {
        documented: "`<runtime>/server.sock`",
        source: "crates/iznik-server/src/daemon/socket.rs",
        named: "pub const NAME: &str = \"server.sock\";",
    },
    Promise {
        documented: "`<runtime>/server.lock`",
        source: "crates/iznik-server/src/daemon/mod.rs",
        named: "const LOCK_NAME: &str = \"server.lock\";",
    },
    Promise {
        documented: "`<runtime>/server.log`",
        source: "crates/iznik-server/src/daemon/mod.rs",
        named: "const LOG_NAME: &str = \"server.log\";",
    },
];

/// The workspace root, from this crate's own directory.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The section of the README this is about.
///
/// # Errors
///
/// When the README cannot be read or has no such section.
fn section() -> Result<String, Failed> {
    let held = std::fs::read_to_string(root().join("README.md"))?;
    let at = held
        .find(SECTION)
        .ok_or_else(|| format!("the README has no {SECTION:?}"))?;
    let rest = held.get(at.saturating_add(SECTION.len())..).unwrap_or("");
    let ends = rest.find("\n## ").unwrap_or(rest.len());
    Ok(rest.get(..ends).unwrap_or(rest).to_owned())
}

/// # Panics
///
/// When a path the README promises is not in the source that writes it.
#[test]
fn documented_paths_are_the_ones_the_code_writes() {
    let case = || -> Result<(), Failed> {
        let said = section()?;
        for promise in PROMISES {
            assert!(
                said.contains(promise.documented),
                "the README names {}",
                promise.documented
            );
            let source = std::fs::read_to_string(root().join(promise.source))?;
            assert!(
                source.contains(promise.named),
                "{} says {} for {}",
                promise.source,
                promise.named,
                promise.documented
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the section names a path nothing in the table accounts for.
#[test]
fn documented_paths_leave_nothing_unaccounted_for() {
    let case = || -> Result<(), Failed> {
        let said = section()?;
        // Every code span in the section that looks like a path this program
        // makes. A path added to the document with nothing behind it is the
        // failure this catches.
        for span in said.split('`').skip(1).step_by(2) {
            let looks_like_a_path = span.contains('/') && !span.contains(' ');
            if !looks_like_a_path {
                continue;
            }
            assert!(
                PROMISES
                    .iter()
                    .any(|promise| promise.documented.trim_matches('`') == span),
                "{span:?} is named in the README and in nothing this holds to the code"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
