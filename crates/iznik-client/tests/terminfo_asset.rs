//! The committed terminfo, compiled by the same `tic` a host would use.
//!
//! What matters about this asset is not its bytes but what a program on the
//! host learns from it, so every case here compiles it and asks the compiled
//! entry — with `tic` and `infocmp` from the machine's own ncurses, which is
//! what the host image carries too.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use iznik_client::bootstrap::terminfo::{TERMINAL_NAME, XTERM_GHOSTTY_TERMINFO};
use iznik_harness::process::{self, Deadline, Output};

/// How long compiling one entry may take. It is milliseconds; this is a bound,
/// not an allowance.
const COMPILE_DEADLINE: Duration = Duration::from_secs(30);

/// What a terminal that can do twenty-four-bit colour says about itself.
const TRUE_COLOR: &str = "Tc";

/// What one that can do styled underlines says.
const STYLED_UNDERLINE: &str = "Smulx";

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

/// A scratch directory named for `case`, holding the asset written out.
///
/// # Errors
///
/// When it cannot be made or written.
fn scratch(case: &str) -> Result<(Scratch, PathBuf), Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-terminfo-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    let source = path.join("xterm-ghostty.terminfo");
    std::fs::write(&source, XTERM_GHOSTTY_TERMINFO)?;
    Ok((Scratch { path }, source))
}

/// Compiles the asset into `held` and gives back what `tic` said.
///
/// # Errors
///
/// When `tic` cannot be run, or does not succeed.
fn compiled(held: &Scratch, source: &PathBuf) -> Result<String, Failed> {
    let mut command = Command::new("tic");
    command
        .arg("-x")
        .arg("-o")
        .arg(held.path.join("terminfo"))
        .arg(source);
    let said = process::run(command, Deadline(COMPILE_DEADLINE), Output::Capture)?;
    Ok(String::from_utf8_lossy(&said.stderr).into_owned())
}

/// # Panics
///
/// When `tic` refuses the asset, or has anything to say about it.
#[test]
fn terminfo_asset_compiles_without_a_word() {
    let case = || -> Result<(), Failed> {
        let (held, source) = scratch("compiles")?;
        let complained = compiled(&held, &source)?;
        assert!(
            complained.trim().is_empty(),
            "tic compiled it and said nothing: {complained}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the compiled entry is not the terminal iznik names, or does not
/// advertise what iznik carries it for.
#[test]
fn terminfo_asset_advertises_what_it_is_carried_for() {
    let case = || -> Result<(), Failed> {
        let (held, source) = scratch("advertises")?;
        let _quiet = compiled(&held, &source)?;
        let mut command = Command::new("infocmp");
        command
            .arg("-x")
            .arg(TERMINAL_NAME)
            .env("TERMINFO", held.path.join("terminfo"));
        let said = process::run(command, Deadline(COMPILE_DEADLINE), Output::Capture)?;
        let entry = String::from_utf8_lossy(&said.stdout).into_owned();
        assert!(
            entry.contains(TERMINAL_NAME),
            "the compiled entry is {TERMINAL_NAME}: {entry}"
        );
        for wanted in [TRUE_COLOR, STYLED_UNDERLINE] {
            assert!(
                entry.contains(wanted),
                "and advertises {wanted}, which is why iznik carries it: {entry}"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the asset does not name the terminal it is for on its first line of
/// content.
#[test]
fn terminfo_asset_names_the_terminal_it_is_for() {
    let named = XTERM_GHOSTTY_TERMINFO
        .lines()
        .find(|line| !line.starts_with('#') && !line.trim().is_empty())
        .unwrap_or_default();
    assert!(
        named.starts_with(TERMINAL_NAME),
        "the entry begins by naming {TERMINAL_NAME}: {named}"
    );
}
