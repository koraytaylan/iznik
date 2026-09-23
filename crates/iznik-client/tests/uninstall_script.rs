//! The scripts that take iznik off a host and put its terminfo on one, run
//! here under `sh`.
//!
//! What matters about them is what they leave: everything of iznik's gone
//! from a prefix iznik made, and nothing but iznik's own files touched in a
//! prefix it was lent — and nothing written through a link somebody else
//! placed where a prefix should be.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use iznik_client::bootstrap::REMOTE_UNINSTALL_SCRIPT;
use iznik_client::bootstrap::upload::REMOTE_TERMINFO_SCRIPT;
use iznik_harness::process::{self, Deadline, Output};

/// How long one run of a script may take.
const RUN_DEADLINE: Duration = Duration::from_secs(30);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A temporary directory of this case's own, removed when the guard drops.
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When it cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-uninstall-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// Writes a file, making the directories above it.
///
/// # Errors
///
/// When it cannot be written.
fn put(path: &Path) -> Result<(), Failed> {
    if let Some(above) = path.parent() {
        std::fs::create_dir_all(above)?;
    }
    std::fs::write(path, b"bytes")?;
    Ok(())
}

/// Runs `script` under `sh` with `prefix` as the prefix, and runtime
/// directories kept inside `held`.
///
/// # Errors
///
/// When it cannot be run, or exits other than cleanly.
fn run(script: &str, prefix: &Path, held: &Scratch, input: Stdio) -> Result<(), Failed> {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .env("IZNIK_PREFIX", prefix)
        .env("XDG_RUNTIME_DIR", held.path.join("xdg"))
        .env("TMPDIR", held.path.join("temporary"))
        .stdin(input);
    let _done = process::run(command, Deadline(RUN_DEADLINE), Output::Capture)?;
    Ok(())
}

/// # Panics
///
/// When taking iznik off a prefix iznik made leaves anything of it behind.
#[test]
fn uninstall_script_removes_a_prefix_of_its_own() {
    let case = || -> Result<(), Failed> {
        let held = scratch("own")?;
        let prefix = held.path.join("data").join("iznik");
        put(&prefix.join("bin/iznik-server"))?;
        put(&prefix.join("bin/iznik-server.sha256"))?;
        put(&prefix.join("terminfo/x/xterm-ghostty"))?;
        run(REMOTE_UNINSTALL_SCRIPT, &prefix, &held, Stdio::null())?;
        assert!(!prefix.exists(), "the prefix iznik made is gone");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When taking iznik off a prefix it was lent removes anything but iznik's own
/// files, or leaves one of them.
#[test]
fn uninstall_script_takes_only_its_own_from_a_lent_prefix() {
    let case = || -> Result<(), Failed> {
        let held = scratch("lent")?;
        let prefix = held.path.join("xdg");
        let ours = [
            prefix.join("bin/iznik-server"),
            prefix.join("bin/iznik-server.sha256"),
            prefix.join("terminfo/x/xterm-ghostty"),
        ];
        let theirs = [
            prefix.join("bin/their-tool"),
            prefix.join("terminfo/x/their-terminal"),
        ];
        for file in ours.iter().chain(theirs.iter()) {
            put(file)?;
        }
        std::fs::create_dir_all(prefix.join("empty-of-theirs"))?;
        run(REMOTE_UNINSTALL_SCRIPT, &prefix, &held, Stdio::null())?;
        for file in &ours {
            assert!(!file.exists(), "iznik's own {} is gone", file.display());
        }
        for file in &theirs {
            assert!(file.exists(), "somebody else's {} is not", file.display());
        }
        assert!(
            prefix.join("empty-of-theirs").is_dir(),
            "and no directory of theirs is removed, empty or not"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the terminfo script writes through a prefix that is a link somebody
/// else placed.
#[test]
fn terminfo_script_refuses_a_prefix_that_is_a_link() {
    let case = || -> Result<(), Failed> {
        let held = scratch("linked")?;
        let elsewhere = held.path.join("elsewhere");
        std::fs::create_dir_all(&elsewhere)?;
        let prefix = held.path.join("iznik");
        std::os::unix::fs::symlink(&elsewhere, &prefix)?;
        let refused = run(REMOTE_TERMINFO_SCRIPT, &prefix, &held, Stdio::null());
        assert!(refused.is_err(), "a linked prefix is refused");
        assert_eq!(
            std::fs::read_dir(&elsewhere)?.count(),
            0,
            "and nothing is written where it points"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
