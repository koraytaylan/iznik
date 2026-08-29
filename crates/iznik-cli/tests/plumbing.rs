//! The plumbing, run the way a script runs it.
//!
//! Every case here spawns the real binary with its standard input closed and
//! reads what it printed, because what is being asked is exactly what a shell
//! would get: one JSON object a line on standard output, nothing on standard
//! error, and a status that says whether to believe it. The lines are read
//! back with a real parser rather than with the writer that made them.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::PaneId;
use iznik_testkit::client::TestClient;
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a command that talks to a daemon on this machine may take.
const PROMPT: Duration = Duration::from_mins(1);

/// How long `tail` is left running before it is interrupted.
const TAILING: Duration = Duration::from_secs(2);

/// The exit code a refused command line gives.
const USAGE_EXIT_CODE: i32 = 2;

/// The width the session these cases make is made at.
const COLUMNS: u16 = 80;

/// And its height.
const ROWS: u16 = 24;

/// The pane that session comes with.
const PANE: PaneId = PaneId(1);

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
    let path = base.join(format!("iznik-plumbing-{case}-{}", std::process::id()));
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

/// What one run of the binary said.
#[derive(Debug)]
struct Ran {
    /// Its status.
    code: Option<i32>,
    /// What it printed.
    stdout: String,
    /// And what it complained.
    stderr: String,
}

/// The binary, with a runtime directory of this case's own and nothing on its
/// standard input: no command here may read a terminal or wait for one.
fn binary(held: &Scratch, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iznik"));
    command
        .args(arguments)
        .env("XDG_RUNTIME_DIR", &held.path)
        .env("TMPDIR", &held.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Runs it to completion and says what it said.
///
/// # Errors
///
/// When it cannot be spawned or waited for.
fn ran(held: &Scratch, arguments: &[&str]) -> Result<Ran, Failed> {
    let done = binary(held, arguments).output()?;
    Ok(Ran {
        code: done.status.code(),
        stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
    })
}

/// Every line of some output, read as JSON by a parser that is not the writer
/// that wrote it.
///
/// # Errors
///
/// When a line is not JSON.
///
/// # Panics
///
/// When a line is JSON but is not one object.
fn objects(said: &str) -> Result<Vec<serde_json::Value>, Failed> {
    let mut held = Vec::new();
    for line in said.lines() {
        let value: serde_json::Value = serde_json::from_str(line)?;
        assert!(value.is_object(), "every line is one object: {line}");
        held.push(value);
    }
    Ok(held)
}

/// A daemon on this machine, and the alias that reaches it.
///
/// # Errors
///
/// When it cannot be started.
fn a_daemon(runtime: &Runtime) -> Result<(Stack, String), Failed> {
    let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
    let alias = format!("unix:{}", stack.socket().display());
    Ok((stack, alias))
}

/// # Panics
///
/// When a command that cannot work says so any way but the one every command
/// says it.
#[test]
fn plumbing_says_what_refused_and_why() {
    let case = || -> Result<(), Failed> {
        let held = scratch("refused")?;
        // A socket is not a host to run a probe on, which is the shortest
        // road to a refusal that comes from the layer below this one.
        let done = ran(&held, &["probe", "unix:/nonexistent/iznik.sock"])?;
        assert_ne!(done.code, Some(0), "a probe of a socket does not succeed");
        assert!(done.stdout.is_empty(), "and prints nothing as an answer");
        let said = objects(&done.stderr)?;
        assert_eq!(said.len(), 1, "one object says what went wrong: {said:?}");
        let first = said.first().ok_or("the one object")?;
        assert!(
            first
                .get("error")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "with the message in it: {first:?}"
        );
        assert_eq!(
            first.get("layer").and_then(serde_json::Value::as_str),
            Some("transport"),
            "and the layer it came from"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a command line the binary cannot act on is not refused as one.
#[test]
fn plumbing_refuses_a_command_line_it_cannot_act_on() {
    let case = || -> Result<(), Failed> {
        let held = scratch("usage")?;
        for arguments in [
            vec!["probe"],
            vec!["state"],
            vec!["tail", "unix:/none.sock"],
            vec!["benchmark", "one", "too", "many"],
        ] {
            let done = ran(&held, &arguments)?;
            assert_eq!(
                done.code,
                Some(USAGE_EXIT_CODE),
                "{arguments:?} is a command line this cannot act on"
            );
            assert!(done.stdout.is_empty(), "nothing is printed as an answer");
            let said = objects(&done.stderr)?;
            assert_eq!(said.len(), 1, "and one object says so: {said:?}");
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the model a host holds is not printed as one object.
#[test]
fn plumbing_prints_the_model_a_host_holds() {
    let case = || -> Result<(), Failed> {
        let held = scratch("state")?;
        let runtime = runtime()?;
        let (stack, alias) = a_daemon(&runtime)?;
        let done = ran(&held, &["state", &alias])?;
        assert_eq!(done.code, Some(0), "state succeeds: {}", done.stderr);
        assert!(
            done.stderr.is_empty(),
            "and says nothing else: {}",
            done.stderr
        );
        let said = objects(&done.stdout)?;
        assert_eq!(said.len(), 1, "one object is the model: {said:?}");
        let model = said.first().ok_or("the one object")?;
        assert_eq!(
            model.get("host").and_then(serde_json::Value::as_str),
            Some(alias.as_str()),
            "naming the host it asked"
        );
        assert!(
            model
                .get("sessions")
                .is_some_and(serde_json::Value::is_array),
            "and carrying its sessions: {model:?}"
        );
        assert!(
            model
                .get("generation")
                .is_some_and(serde_json::Value::is_u64),
            "and which generation of it this is: {model:?}"
        );
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the round trip is not measured, or is printed without its shape.
#[test]
fn plumbing_measures_the_round_trip() {
    let case = || -> Result<(), Failed> {
        let held = scratch("benchmark")?;
        let runtime = runtime()?;
        let (stack, alias) = a_daemon(&runtime)?;
        let began = Instant::now();
        let done = ran(&held, &["benchmark", &alias])?;
        assert!(
            began.elapsed() < PROMPT,
            "inside a minute: {:?}",
            began.elapsed()
        );
        assert_eq!(done.code, Some(0), "benchmark succeeds: {}", done.stderr);
        assert!(
            done.stderr.is_empty(),
            "and says nothing else: {}",
            done.stderr
        );
        let said = objects(&done.stdout)?;
        let shape = said.first().ok_or("the one object")?;
        for named in ["minimum", "median", "ninety_ninth", "maximum"] {
            assert!(
                shape
                    .get(named)
                    .and_then(serde_json::Value::as_f64)
                    .is_some(),
                "{named} is a number: {shape:?}"
            );
        }
        let middle = shape.get("median").and_then(serde_json::Value::as_f64);
        let worst = shape.get("maximum").and_then(serde_json::Value::as_f64);
        assert!(
            middle <= worst,
            "and the middle is not past the end: {middle:?} {worst:?}"
        );
        assert_eq!(
            shape.get("unit").and_then(serde_json::Value::as_str),
            Some("milliseconds"),
            "with what the numbers are in"
        );
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a pane's bytes do not arrive, or an interruption is not an ending.
#[test]
fn plumbing_tails_a_pane_until_it_is_interrupted() {
    let case = || -> Result<(), Failed> {
        let held = scratch("tail")?;
        let runtime = runtime()?;
        let (stack, alias) = a_daemon(&runtime)?;
        // A session, so that there is a pane to tail, made by a client that
        // is not the one under test.
        let mut client = runtime.block_on(TestClient::connect(stack.socket()))?;
        let _greeting = runtime.block_on(client.hello(Capabilities::from_bits(0)))?;
        let _made = runtime.block_on(client.command(SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        }))?;
        let tailing = binary(&held, &["tail", &alias, "1"]).spawn()?;
        // Something for the pane to say, typed after the tail has had a
        // moment to attach.
        std::thread::sleep(TAILING);
        runtime.block_on(client.input(PANE, b"echo tailing-42\n".to_vec()))?;
        std::thread::sleep(TAILING);
        interrupt(tailing.id())?;
        let done = tailing.wait_with_output()?;
        assert_eq!(
            done.status.code(),
            Some(0),
            "an interruption is an ending: {}",
            String::from_utf8_lossy(&done.stderr)
        );
        let printed = String::from_utf8_lossy(&done.stdout).into_owned();
        let said = objects(&printed)?;
        assert!(!said.is_empty(), "the pane said something: {printed:?}");
        let first = said.first().ok_or("the first line")?;
        // The pane as it stands comes first, because the bytes after it are
        // changes to it and a reader that skipped it would be reading the
        // middle of something.
        assert_eq!(
            first.get("kind").and_then(serde_json::Value::as_str),
            Some("screen"),
            "the first line is the pane as it stands: {first:?}"
        );
        assert!(
            first.get("columns").is_some_and(serde_json::Value::is_u64)
                && first.get("rows").is_some_and(serde_json::Value::is_u64),
            "with the size it was drawn at: {first:?}"
        );
        assert_eq!(
            first.get("encoding").and_then(serde_json::Value::as_str),
            Some("base64"),
            "and said how it is written: {first:?}"
        );
        let output = said
            .iter()
            .find(|line| line.get("kind").and_then(serde_json::Value::as_str) == Some("output"))
            .ok_or("what the pane said after it")?;
        assert!(
            output
                .get("bytes")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|written| !written.is_empty()),
            "and what was typed came back: {output:?}"
        );
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Interrupts a child, the way a person at a terminal would.
///
/// # Errors
///
/// When the signal cannot be sent.
///
/// # Panics
///
/// When it is not delivered.
fn interrupt(child: u32) -> Result<(), Failed> {
    let done = Command::new("kill")
        .arg("-INT")
        .arg(child.to_string())
        .status()?;
    assert!(done.success(), "the signal was sent");
    Ok(())
}
