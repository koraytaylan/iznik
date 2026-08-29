//! The bundle, and what it must never carry.
//!
//! One document with every section in it, so that a layer which failed is
//! visible by having an error where its answer belongs — and so that the
//! layers under it say they were skipped rather than pretending to have been
//! asked. The redaction case is the one that matters most: sentinels are
//! planted where a careless collector would pick them up, and the bundle is
//! scanned for every one of them.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long the bundle may take against a daemon on this machine.
const PROMPT: Duration = Duration::from_mins(2);

/// What is planted where a careless collector would find it, and must not
/// appear anywhere in the bundle.
const SENTINELS: [&str; 4] = [
    "SENTINEL-PRIVATE-KEY-MATERIAL",
    "SENTINEL-AGENT-SOCKET",
    "SENTINEL-TOKEN-VALUE",
    "SENTINEL-PASSPHRASE",
];

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A temporary directory of this case's own, removed when the guard drops.
#[derive(Debug)]
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
    let path = base.join(format!("iznik-doctor-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the daemons these cases stand up.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// Runs `iznik doctor` and gives back what it printed.
///
/// # Errors
///
/// When it cannot be spawned or waited for.
fn ran(held: &Scratch, alias: &str, planted: bool) -> Result<(String, String), Failed> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iznik"));
    command
        .args(["doctor", alias])
        .env("XDG_RUNTIME_DIR", &held.path)
        .env("TMPDIR", &held.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if planted {
        // Where a collector that read the environment would find them, and
        // where one that expanded an ssh configuration would.
        command
            .env("IZNIK_TOKEN", SENTINELS[2])
            .env("SSH_AUTH_SOCK", format!("/tmp/{}", SENTINELS[1]))
            .env("HOME", &held.path);
    }
    let done = command.output()?;
    Ok((
        String::from_utf8_lossy(&done.stdout).into_owned(),
        String::from_utf8_lossy(&done.stderr).into_owned(),
    ))
}

/// What `ssh -G` says for a host whose configuration names everything a
/// bundle must never carry.
///
/// Given to the filter directly rather than through `ssh`, which finds a
/// user's configuration from the account and not from the environment — so a
/// case cannot point it at a planted one without touching the real one, and
/// what is being asked here is what the filter keeps.
fn expanded() -> String {
    format!(
        "host planted-host\n\
         hostname 127.0.0.1\n\
         user planted\n\
         port 22\n\
         batchmode yes\n\
         compression no\n\
         identityfile /home/somebody/.ssh/{}\n\
         identityagent /tmp/{}\n\
         proxycommand /bin/echo {}\n\
         passphrase {}\n",
        SENTINELS[0], SENTINELS[1], SENTINELS[2], SENTINELS[3]
    )
}

/// The bundle as JSON, read by a parser that is not the writer that wrote it.
///
/// # Errors
///
/// When it is not one JSON object.
///
/// # Panics
///
/// When it is JSON but not an object.
fn document(said: &str) -> Result<serde_json::Value, Failed> {
    let line = said.lines().next().ok_or("the bundle is one line")?;
    let held: serde_json::Value = serde_json::from_str(line)?;
    assert!(held.is_object(), "the bundle is one object: {line}");
    Ok(held)
}

/// # Panics
///
/// When a section is missing, or a filled one carries an error.
#[test]
fn doctor_carries_every_section_for_a_host_that_answers() {
    let case = || -> Result<(), Failed> {
        let held = scratch("healthy")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let alias = format!("unix:{}", stack.socket().display());
        let began = Instant::now();
        let (printed, complained) = ran(&held, &alias, false)?;
        assert!(began.elapsed() < PROMPT, "inside {PROMPT:?}");
        assert!(complained.is_empty(), "nothing is complained: {complained}");
        let bundle = document(&printed)?;
        for named in [
            "client",
            "ssh",
            "probe",
            "server",
            "host_state",
            "round_trip",
        ] {
            assert!(
                bundle.get(named).is_some(),
                "the {named} section is there: {bundle:?}"
            );
        }
        // A host that answers: its server said what it is, and a keystroke
        // was measured against it.
        let server = bundle.get("server").ok_or("the server section")?;
        assert!(
            server
                .get("version")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|said| !said.is_empty()),
            "the server said what it is: {server:?}"
        );
        let trip = bundle.get("round_trip").ok_or("the round trip")?;
        assert!(
            trip.get("median")
                .and_then(serde_json::Value::as_f64)
                .is_some(),
            "and a keystroke was measured: {trip:?}"
        );
        // The two that cannot be asked of a socket say so, rather than
        // carrying an error that would read as a fault.
        for named in ["ssh", "probe"] {
            let section = bundle.get(named).ok_or("the section")?;
            assert!(
                section.get("skipped").is_some(),
                "{named} says it was not asked: {section:?}"
            );
        }
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a host that cannot be reached does not say so where its answer
/// belongs, or when the sections under it pretend to have been asked.
#[test]
fn doctor_says_which_layer_failed_for_a_host_that_does_not_answer() {
    let case = || -> Result<(), Failed> {
        let held = scratch("unreachable")?;
        // A socket that is not there: nothing to connect to, and nothing to
        // mistake for a slow host.
        let alias = format!("unix:{}", held.path.join("nothing.sock").display());
        let (printed, _complained) = ran(&held, &alias, false)?;
        let bundle = document(&printed)?;
        let server = bundle.get("server").ok_or("the server section")?;
        assert_eq!(
            server.get("layer").and_then(serde_json::Value::as_str),
            Some("transport"),
            "the layer that failed is named: {server:?}"
        );
        let state = bundle.get("host_state").ok_or("the state section")?;
        assert!(
            state.get("went_through").is_some(),
            "with every state it went through: {state:?}"
        );
        let trip = bundle.get("round_trip").ok_or("the round trip")?;
        assert!(
            trip.get("skipped").is_some(),
            "and what could not be asked says so rather than failing: {trip:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a setting this does not name is kept, or one it names is not.
#[test]
fn doctor_keeps_only_the_settings_it_names() {
    let said = expanded();
    let mut written = Vec::new();
    iznik_cli::output::line(&mut written, &iznik_cli::doctor::kept(&said))
        .unwrap_or_else(|error| panic!("{error}"));
    let printed = String::from_utf8_lossy(&written).into_owned();
    for sentinel in SENTINELS {
        assert!(
            !printed.contains(sentinel),
            "{sentinel} is not kept: {printed}"
        );
    }
    // And what it does keep is there, so the case is not passing because it
    // kept nothing at all.
    for named in ["127.0.0.1", "planted", "yes"] {
        assert!(printed.contains(named), "{named} is kept: {printed}");
    }
}

/// # Panics
///
/// When anything planted where a careless collector would find it appears in
/// the bundle.
#[test]
fn doctor_carries_no_secret_it_was_given_the_chance_to_carry() {
    let case = || -> Result<(), Failed> {
        let held = scratch("redaction")?;
        let runtime = runtime()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let alias = format!("unix:{}", stack.socket().display());
        // Sentinels in the environment, which is where a collector that read
        // one would find them — and this one never reads one.
        let (printed, complained) = ran(&held, &alias, true)?;
        let said = format!("{printed}{complained}");
        for sentinel in SENTINELS {
            assert!(
                !said.contains(sentinel),
                "{sentinel} is nowhere in what the bundle said: {said}"
            );
        }
        // And the bundle was really made, so the case is not passing on an
        // empty document.
        let bundle = document(&printed)?;
        assert!(
            bundle.get("server").is_some(),
            "the bundle was made: {bundle:?}"
        );
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
