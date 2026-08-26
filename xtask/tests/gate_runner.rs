//! The gate runner: the table `xtask::gate` holds agrees with the one Makina
//! runs, `check` stops at the first missing prerequisite or failing gate
//! naming it, and with everything passing it prints one line per gate.
//!
//! The gates are not run for real here — that is the done-when's job and
//! takes minutes. Every tool the doctor probes and every `cargo` the gates
//! run is a shim in a directory that is the whole `PATH` of the run, which
//! records what it was asked to do and fails on request, so what is tested
//! is the runner's logic and nothing about this machine.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{Completed, Deadline, Output, ProcessError, run};
use xtask::doctor::pinned_channel;
use xtask::gate::{GATES, Gate, PROGRAM};
use xtask::repository_root;

/// How long a run of the `xtask` binary may take here: every gate and probe
/// is a shim that exits at once.
const RUN_DEADLINE: Duration = Duration::from_secs(20);

/// The mode of a shim: executable by everyone.
const EXECUTABLE_MODE: u32 = 0o755;

/// The variable naming the gate whose `cargo` shim fails.
const FAILING_GATE_VARIABLE: &str = "IZNIK_SHIM_FAILING_GATE";

/// The variable naming the file the `cargo` shim appends the gates it ran to,
/// each with the `RUSTDOCFLAGS` it saw when it saw any.
const SHIM_LOG_VARIABLE: &str = "IZNIK_SHIM_LOG";

/// The tools other than `cargo` the doctor probes, each shimmed to succeed
/// and print what the doctor expects.
const TOOL_SHIMS: &[(&str, &str)] = &[
    ("podman", "netavark"),
    ("zig", "0.15.2"),
    ("x86_64-linux-musl-gcc", "shim"),
    ("aarch64-linux-musl-gcc", "shim"),
    ("git", "git version shim"),
];

/// The lines the `cargo` shim logs when every gate runs: the documentation
/// gate's line carries the environment it was given.
const EVERY_GATE_LOGGED: &[&str] = &[
    "format",
    "lint",
    "documentation RUSTDOCFLAGS=-D warnings",
    "test",
    "claims",
];

/// A directory of shims that is a `PATH` on its own, removed when dropped.
#[derive(Debug)]
struct Shims {
    /// The directory.
    directory: PathBuf,
    /// The file the `cargo` shim logs the gates it ran to.
    log: PathBuf,
}

impl Shims {
    /// Writes every shim: a `cargo` that answers the doctor's version probes,
    /// logs each gate it is asked to run and fails the one named by
    /// [`FAILING_GATE_VARIABLE`], and one shim per other tool.
    ///
    /// # Errors
    ///
    /// When a file cannot be written.
    fn new(name: &str) -> Result<Shims, String> {
        let channel = pinned_channel(&repository_root()).map_err(|error| error.to_string())?;
        let directory =
            env::temp_dir().join(format!("iznik-gate-runner-{name}-{}", std::process::id()));
        fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let log = directory.join("gates.log");
        let cargo = format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
               --version) echo \"cargo {channel} (shim)\"; exit 0;;\n\
               nextest) if [ \"$2\" = --version ]; then echo \"cargo-nextest (shim)\"; exit 0; fi; gate=test;;\n\
               fmt) gate=format;;\n\
               clippy) gate=lint;;\n\
               doc) gate=documentation;;\n\
               xtask) gate=claims;;\n\
               *) echo \"unexpected cargo $*\" >&2; exit 9;;\n\
             esac\n\
             echo \"$gate${{RUSTDOCFLAGS:+ RUSTDOCFLAGS=$RUSTDOCFLAGS}}\" >> \"${SHIM_LOG_VARIABLE}\"\n\
             [ \"${FAILING_GATE_VARIABLE}\" = \"$gate\" ] && exit 1\n\
             exit 0\n"
        );
        let shims = Shims { directory, log };
        shims.write(PROGRAM, &cargo)?;
        for (tool, output) in TOOL_SHIMS {
            shims.write(tool, &format!("#!/bin/sh\necho '{output}'\n"))?;
        }
        Ok(shims)
    }

    /// Writes one executable shim.
    ///
    /// # Errors
    ///
    /// When the file cannot be written or made executable.
    fn write(&self, tool: &str, script: &str) -> Result<(), String> {
        let path = self.directory.join(tool);
        fs::write(&path, script).map_err(|error| format!("writing {}: {error}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(EXECUTABLE_MODE))
            .map_err(|error| format!("chmod {}: {error}", path.display()))
    }

    /// Removes one shim, so the tool is absent from the `PATH`.
    ///
    /// # Errors
    ///
    /// When the file cannot be removed.
    fn remove(&self, tool: &str) -> Result<(), String> {
        let path = self.directory.join(tool);
        fs::remove_file(&path).map_err(|error| format!("removing {}: {error}", path.display()))
    }

    /// The gates the `cargo` shim was asked to run, in order.
    ///
    /// # Errors
    ///
    /// When the log cannot be read.
    fn gates_run(&self) -> Result<Vec<String>, String> {
        let text = fs::read_to_string(&self.log)
            .map_err(|error| format!("reading {}: {error}", self.log.display()))?;
        Ok(text.lines().map(str::to_owned).collect())
    }
}

impl Drop for Shims {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap_or_default();
    }
}

/// Runs the `xtask` binary with the shim directory as its whole `PATH` and the
/// failing gate named when one is, through `iznik_harness::process::run`.
///
/// # Errors
///
/// Whatever the runner reports: a usage error or a failed check is
/// [`ProcessError::Failed`].
fn run_xtask(
    shims: &Shims,
    arguments: &[&str],
    failing_gate: Option<&str>,
) -> Result<Completed, ProcessError> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .args(arguments)
        .env("PATH", &shims.directory)
        .env_remove("RUSTDOCFLAGS")
        .env(SHIM_LOG_VARIABLE, &shims.log);
    if let Some(gate) = failing_gate {
        command.env(FAILING_GATE_VARIABLE, gate);
    }
    run(command, Deadline(RUN_DEADLINE), Output::Capture)
}

/// The words of a shell command line, with single-quoted arguments honoured.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut pending = false;
    for character in line.chars() {
        match character {
            '\'' => {
                quoted = !quoted;
                pending = true;
            }
            space if space.is_whitespace() && !quoted => {
                if pending {
                    words.push(std::mem::take(&mut word));
                    pending = false;
                }
            }
            other => {
                word.push(other);
                pending = true;
            }
        }
    }
    if pending {
        words.push(word);
    }
    words
}

/// One Makina gate as its configuration spells it: name, environment,
/// deadline in seconds, and command words.
#[derive(Debug, PartialEq, Eq)]
struct MakinaGate {
    /// The gate's name.
    name: String,
    /// The variables set before the command.
    environment: Vec<(String, String)>,
    /// The seconds after `timeout`.
    deadline_seconds: u64,
    /// The command after `timeout <seconds>`.
    command: Vec<String>,
}

/// Parses one `[[gates]]` command: everything after the shared
/// `export …;` prefix is `NAME=value` assignments, then `timeout <seconds>`,
/// then the command.
///
/// # Errors
///
/// When the command line is not of that shape.
fn parse_makina_gate(name: &str, command_line: &str) -> Result<MakinaGate, String> {
    let after_export = command_line.rsplit("; ").next().unwrap_or(command_line);
    let mut words = shell_words(after_export).into_iter().peekable();
    let mut environment = Vec::new();
    while let Some((variable, value)) = words.peek().and_then(|word| word.split_once('=')) {
        environment.push((variable.to_owned(), value.to_owned()));
        words.next();
    }
    if words.next().as_deref() != Some("timeout") {
        return Err(format!(
            "{name}: the command is not under timeout: {after_export}"
        ));
    }
    let deadline_seconds = words
        .next()
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .ok_or_else(|| format!("{name}: timeout without seconds: {after_export}"))?;
    Ok(MakinaGate {
        name: name.to_owned(),
        environment,
        deadline_seconds,
        command: words.collect(),
    })
}

/// The gates `.makina/config.toml` declares, in order.
///
/// # Errors
///
/// When the file cannot be read or a gate is malformed.
fn makina_gates() -> Result<Vec<MakinaGate>, String> {
    let path = repository_root().join(".makina").join("config.toml");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|error| format!("parsing {}: {error}", path.display()))?;
    let gates = table
        .get("gates")
        .and_then(toml::Value::as_array)
        .ok_or("no [[gates]]")?;
    gates
        .iter()
        .map(|gate| {
            let name = gate
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or("a gate without a name")?;
            let command = gate
                .get("command")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| format!("{name}: no command"))?;
            parse_makina_gate(name, command)
        })
        .collect()
}

/// What `xtask::gate` says a gate is, in Makina's terms.
fn table_gate(gate: Gate) -> MakinaGate {
    MakinaGate {
        name: gate.name().to_owned(),
        environment: gate
            .environment()
            .iter()
            .map(|(variable, value)| ((*variable).to_owned(), (*value).to_owned()))
            .collect(),
        deadline_seconds: gate.deadline().as_secs(),
        command: gate
            .command()
            .iter()
            .map(|word| (*word).to_owned())
            .collect(),
    }
}

/// The `Failed` outcome of a run, with the status and the stderr tail.
///
/// # Errors
///
/// When the run is anything else, saying what it was.
fn failure(outcome: Result<Completed, ProcessError>) -> Result<(Option<i32>, String), String> {
    match outcome {
        Err(ProcessError::Failed {
            status,
            stderr_tail,
            ..
        }) => Ok((status.code(), stderr_tail)),
        other => Err(format!("expected a failure, got {other:?}")),
    }
}

/// The gate table in `xtask::gate` and the `[[gates]]` in
/// `.makina/config.toml` agree on names, order, commands, environment and
/// deadlines — a gate that Makina runs but `check` does not is how "it passed
/// locally" stops meaning anything.
///
/// # Panics
///
/// When the two tables differ.
#[test]
fn gate_runner_table_matches_makina() {
    let makina = makina_gates().expect("the Makina configuration parses");
    let table: Vec<MakinaGate> = GATES.iter().map(|gate| table_gate(*gate)).collect();
    assert_eq!(
        makina, table,
        "the gates Makina runs and the gates check runs"
    );
}

/// Every gate has a name it parses back from, and no two share one.
///
/// # Panics
///
/// When a name does not round-trip.
#[test]
fn gate_runner_names_round_trip() {
    for gate in GATES {
        assert_eq!(Gate::parse(gate.name()), Some(*gate), "{gate} parses back");
    }
    assert_eq!(Gate::parse("no-such-gate"), None, "an unknown name");
}

/// With every gate passing, `check` prints one line per gate with its elapsed
/// time, in order, exits 0, and every gate ran with its environment.
///
/// # Panics
///
/// When the run fails or the lines are not as described.
#[test]
fn gate_runner_check_passes_every_gate_with_one_line_each() {
    let shims = Shims::new("passing").expect("the shims");
    let completed = run_xtask(&shims, &["check"], None).expect("check passes");
    let stdout = String::from_utf8_lossy(&completed.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains(": passed in "))
        .collect();
    let expected: Vec<String> = GATES
        .iter()
        .map(|gate| format!("{}: passed in ", gate.name()))
        .collect();
    assert_eq!(lines.len(), expected.len(), "one line per gate: {stdout}");
    for (line, prefix) in lines.iter().zip(&expected) {
        assert!(
            line.starts_with(prefix),
            "{line} does not begin with {prefix}"
        );
        assert!(
            line.ends_with('s'),
            "{line} does not end with a time in seconds"
        );
    }
    assert_eq!(
        shims.gates_run().expect("the shim log"),
        EVERY_GATE_LOGGED,
        "every gate ran, in order, with its environment"
    );
}

/// `check` stops at the first failing gate, names it and its command, and
/// runs nothing after it.
///
/// # Panics
///
/// When the run passes, does not name the gate, or ran a later gate.
#[test]
fn gate_runner_check_stops_at_the_first_failing_gate() {
    let shims = Shims::new("failing").expect("the shims");
    let (status, stderr) = failure(run_xtask(&shims, &["check"], Some("lint"))).expect("a failure");
    assert_eq!(status, Some(1), "the failure status");
    assert!(
        stderr.contains("gate lint failed running `cargo clippy"),
        "stderr names the gate and its command: {stderr}"
    );
    assert_eq!(
        shims.gates_run().expect("the shim log"),
        ["format", "lint"],
        "the gates that ran"
    );
}

/// `check` stops before any gate when a prerequisite is absent from the
/// `PATH`, and names it with its install hint.
///
/// # Panics
///
/// When the run passes, a gate ran, or the prerequisite is not named.
#[test]
fn gate_runner_check_stops_at_a_missing_prerequisite() {
    let shims = Shims::new("prerequisite").expect("the shims");
    shims.remove("zig").expect("zig removed from the PATH");
    let (status, stderr) = failure(run_xtask(&shims, &["check"], None)).expect("a failure");
    assert_eq!(status, Some(1), "the failure status");
    assert!(
        stderr.contains("prerequisite missing: zig: `zig` is not on PATH"),
        "stderr names the prerequisite and why: {stderr}"
    );
    assert!(
        stderr.contains("install: install zig from https://ziglang.org/download/"),
        "stderr carries the install hint: {stderr}"
    );
    assert!(!shims.log.exists(), "no gate ran");
}

/// `xtask gate <name>` runs that one gate and prints its line; an unknown
/// name, or an extra argument, is a usage error.
///
/// # Panics
///
/// When the gate does not run alone or a usage error is missing.
#[test]
fn gate_runner_gate_runs_one_gate() {
    let shims = Shims::new("one").expect("the shims");
    let completed = run_xtask(&shims, &["gate", "documentation"], None).expect("the gate passes");
    let stdout = String::from_utf8_lossy(&completed.stdout);
    assert!(
        stdout.contains("documentation: passed in "),
        "the gate's line: {stdout}"
    );
    assert_eq!(
        shims.gates_run().expect("the shim log"),
        ["documentation RUSTDOCFLAGS=-D warnings"],
        "only that gate ran, with its environment"
    );
    for arguments in [
        &["gate", "no-such-gate"][..],
        &["gate", "lint", "extra"][..],
    ] {
        let (status, _stderr) = failure(run_xtask(&shims, arguments, None)).expect("a failure");
        assert_eq!(
            status,
            Some(i32::from(xtask::USAGE_EXIT_CODE)),
            "{arguments:?} is a usage error"
        );
    }
    assert_eq!(
        shims.gates_run().expect("the shim log"),
        ["documentation RUSTDOCFLAGS=-D warnings"],
        "the usage errors ran no gate"
    );
}
