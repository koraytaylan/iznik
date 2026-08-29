//! The two-container fixture under podman, ignored by default: both
//! containers answer by name as the unprivileged user, the start stays under
//! its ceiling, SSH works with generated credentials only, two hosts are
//! distinct, faults do what they say, teardown leaves nothing, the reaper
//! removes a dead owner's leavings, the readiness cap fails cleanly, and
//! staging is a no-op the second time and honours its override.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use iznik_harness::deadline::wait_until;
use iznik_harness::fixture::{
    ENGINE_ALIAS, FIXTURE_START_CEILING, Fault, Fixture, FixtureError, FixtureOptions,
    IDLE_PROGRAM, OWNER_LABEL, Process, RUN_LABEL, Reap, reap,
};
use iznik_harness::images::{IMAGE_BUILD_DEADLINE, PROGRAM, ensure_images};
use iznik_harness::process::{self, Completed, Deadline, Output, ProcessError};
use iznik_harness::staging::{STAGING_DEADLINE, StagingOptions, stage, stage_with};

/// How long one command in a container may take.
const COMMAND: Duration = Duration::from_secs(10);

/// How long a disconnected host may take to refuse: over the connect
/// timeout, under the inventory's five seconds.
const REFUSAL: Duration = Duration::from_secs(5);

/// How long a reconnected host may take to answer again.
const RECOVERY: Duration = Duration::from_secs(5);

/// How often recovery is asked.
const RECOVERY_INTERVAL: Duration = Duration::from_millis(200);

/// How long a printing process is watched between two looks.
const WATCH: Duration = Duration::from_millis(300);

/// The readiness cap the failing-host test sets.
const SHORT_CAP: Duration = Duration::from_secs(2);

/// How long staging that is a no-op may take: generous, because the suite's
/// other tests build through the same cargo lock and a no-op waits its turn.
const RESTAGE_BOUND: Duration = Duration::from_secs(30);

/// The seconds a hand-started container lives at most.
const CONTAINER_LIFETIME: &str = "120";

/// The engine's own command deadline for the census.
const CENSUS: Duration = Duration::from_mins(1);

/// One engine command, captured.
///
/// # Errors
///
/// The process runner's error.
fn podman(arguments: &[&str]) -> Result<Completed, ProcessError> {
    let mut command = Command::new(PROGRAM);
    command.args(arguments);
    process::run(command, Deadline(CENSUS), Output::Capture)
}

/// The standard output of a completed command as text.
fn text(completed: &Completed) -> String {
    String::from_utf8_lossy(&completed.stdout).into_owned()
}

/// The staged directory, built when it must be.
///
/// # Errors
///
/// Staging's error.
fn staged() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(stage(Deadline(STAGING_DEADLINE))?)
}

/// A started fixture with some hosts.
///
/// # Errors
///
/// Staging's or the fixture's error.
fn started(hosts: usize) -> Result<Fixture, Box<dyn std::error::Error>> {
    Ok(Fixture::start(FixtureOptions::new(hosts, staged()?))?)
}

/// Everything podman still holds with a run's prefix: containers, then
/// networks.
///
/// # Errors
///
/// The process runner's error.
fn leavings(prefix: &str) -> Result<Vec<String>, ProcessError> {
    let containers = podman(&["ps", "--all", "--format", "{{.Names}}"])?;
    let networks = podman(&["network", "ls", "--format", "{{.Name}}"])?;
    Ok(text(&containers)
        .lines()
        .chain(text(&networks).lines())
        .filter(|name| name.starts_with(prefix))
        .map(str::to_owned)
        .collect())
}

/// Both containers are reachable: `exec` on the engine and on `host0`
/// each return the container's hostname, running as uid 1000.
///
/// # Panics
///
/// When podman is missing (the message names it) or a container does not
/// answer as described.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_both_containers_answer_by_name() {
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    for alias in [ENGINE_ALIAS, &Fixture::host_alias(0)] {
        let answer = fixture
            .exec(alias, "hostname; id -u", COMMAND)
            .unwrap_or_else(|error| panic!("{alias} did not answer: {error}"));
        assert_eq!(text(&answer), format!("{alias}\n1000\n"));
    }
}

/// The start stays under `FIXTURE_START_CEILING` with warm images; the
/// measured time is printed. Named for nextest's latency rule, so it has
/// the machine to itself.
///
/// # Panics
///
/// When podman is missing (the message names it) or the start is slower
/// than the ceiling.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_start_latency_is_under_the_ceiling() {
    ensure_images(Deadline(IMAGE_BUILD_DEADLINE))
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let elapsed = fixture.elapsed_start();
    let _written = writeln!(
        std::io::stdout(),
        "fixture start: {elapsed:?} (ceiling {FIXTURE_START_CEILING:?})"
    );
    assert!(
        elapsed < FIXTURE_START_CEILING,
        "the fixture took {elapsed:?} to start"
    );
}

/// Real SSH with generated credentials only: `ssh host0 hostname` succeeds
/// with `BatchMode=yes`, and the engine has no agent socket and no key the
/// fixture did not generate.
///
/// # Panics
///
/// When podman is missing (the message names it), SSH fails, an agent
/// socket exists, or a foreign key does.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_ssh_uses_generated_credentials_only() {
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let over_ssh = fixture
        .exec(ENGINE_ALIAS, "ssh -o BatchMode=yes host0 hostname", COMMAND)
        .unwrap_or_else(|error| panic!("ssh to host0 failed: {error}"));
    assert_eq!(text(&over_ssh).trim(), "host0");
    let keys = fixture
        .exec(
            ENGINE_ALIAS,
            "test -z \"$SSH_AUTH_SOCK\" && ls ~/.ssh",
            COMMAND,
        )
        .unwrap_or_else(|error| panic!("the engine has an agent socket: {error}"));
    let listed: Vec<String> = text(&keys).lines().map(str::to_owned).collect();
    let identities: Vec<&String> = listed
        .iter()
        .filter(|name| name.starts_with("id_") && !name.contains('.'))
        .collect();
    assert_eq!(
        identities,
        [&"id_ed25519".to_owned()],
        "the engine holds a private key the fixture did not generate: {listed:?}"
    );
}

/// With `hosts = 2`, both aliases resolve by name and the two host
/// containers are distinct.
///
/// # Panics
///
/// When podman is missing (the message names it) or an alias does not
/// answer with its own name.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_two_hosts_are_distinct() {
    let fixture = started(2)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    for index in 0..2 {
        let alias = Fixture::host_alias(index);
        let answer = fixture
            .exec(
                ENGINE_ALIAS,
                &format!("ssh -o BatchMode=yes {alias} hostname"),
                COMMAND,
            )
            .unwrap_or_else(|error| panic!("ssh to {alias} failed: {error}"));
        assert_eq!(text(&answer).trim(), alias);
    }
}

/// After `DisconnectNetwork`, `ssh host0 true` fails within five seconds;
/// after `ReconnectNetwork`, it succeeds again by name.
///
/// # Panics
///
/// When podman is missing (the message names it), the disconnected host
/// still answers or takes over five seconds to refuse, or the reconnected
/// host does not answer again.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_network_faults_disconnect_and_reconnect() {
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let host = Fixture::host_alias(0);
    fixture
        .fault(&Fault::DisconnectNetwork {
            container: host.clone(),
        })
        .expect("disconnects");
    let started = Instant::now();
    let refused = fixture.exec(ENGINE_ALIAS, "ssh -o BatchMode=yes host0 true", COMMAND);
    let elapsed = started.elapsed();
    assert!(refused.is_err(), "the disconnected host answered");
    assert!(elapsed < REFUSAL, "the refusal took {elapsed:?}");
    fixture
        .fault(&Fault::ReconnectNetwork {
            container: host.clone(),
        })
        .expect("reconnects");
    wait_until(RECOVERY, RECOVERY_INTERVAL, "host0 answering again", || {
        fixture
            .exec(ENGINE_ALIAS, "ssh -o BatchMode=yes host0 true", COMMAND)
            .is_ok()
    })
    .expect("the reconnected host answers by name");
}

/// After `PauseProcess` on a process that is printing, its output stops;
/// after `ResumeProcess`, it continues; after `KillProcess` through an
/// `IdFile` the process wrote, it is gone from the census.
///
/// # Panics
///
/// When podman is missing (the message names it), or a fault does not do
/// what it says.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_process_faults_pause_resume_and_kill() {
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let host = Fixture::host_alias(0);
    let start = "nohup sh -c 'echo $$ > /tmp/printer.pid; while :; do date +%s%N >> /tmp/printer.out; sleep 0.05; done' >/dev/null 2>&1 &";
    fixture
        .exec(&host, start, COMMAND)
        .expect("the printer starts");
    let size = || {
        fixture
            .exec(&host, "wc -c < /tmp/printer.out", COMMAND)
            .map(|completed| text(&completed).trim().to_owned())
            .unwrap_or_default()
    };
    let process = Process::IdFile(PathBuf::from("/tmp/printer.pid"));
    thread::sleep(WATCH);
    let before_pause = size();
    fixture
        .fault(&Fault::PauseProcess {
            container: host.clone(),
            process: process.clone(),
        })
        .expect("pauses");
    thread::sleep(WATCH);
    let paused = size();
    thread::sleep(WATCH);
    assert_eq!(size(), paused, "the paused process kept printing");
    assert_ne!(
        before_pause, "",
        "the printer printed nothing before the pause"
    );
    fixture
        .fault(&Fault::ResumeProcess {
            container: host.clone(),
            process: process.clone(),
        })
        .expect("resumes");
    thread::sleep(WATCH);
    assert_ne!(size(), paused, "the resumed process did not print");
    fixture
        .fault(&Fault::KillProcess {
            container: host.clone(),
            process,
        })
        .expect("kills");
    thread::sleep(WATCH);
    let census = fixture
        .exec(
            &host,
            "kill -0 $(cat /tmp/printer.pid) 2>/dev/null && echo alive || echo gone",
            COMMAND,
        )
        .expect("the census answers");
    assert_eq!(text(&census).trim(), "gone");
}

/// After a fixture is dropped normally and after a test body panics,
/// nothing with the run's prefix remains.
///
/// # Panics
///
/// When podman is missing (the message names it) or something remains.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_teardown_leaves_nothing_behind() {
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let prefix = fixture.prefix().to_owned();
    drop(fixture);
    assert_eq!(
        leavings(&prefix).expect("the census answers"),
        Vec::<String>::new()
    );
    let panicking = thread::spawn(|| {
        let doomed = started(1)
            .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
        let doomed_prefix = doomed.prefix().to_owned();
        assert!(
            !doomed_prefix.starts_with("iznik-"),
            "a test body panics with its fixture alive: {doomed_prefix}"
        );
        doomed_prefix
    });
    let outcome = panicking.join();
    assert!(outcome.is_err(), "the body did not panic");
    let mine = format!("iznik-{}-", std::process::id());
    assert_eq!(
        leavings(&mine).expect("the census answers"),
        Vec::<String>::new(),
        "a container or network survived the panic"
    );
}

/// A container started by hand with the fixture's labels and a dead owner
/// is removed by the next `Fixture::start`; one whose owner is alive is left
/// alone; `reap(Everything)`, which `xtask regression reap` runs, removes
/// both.
///
/// # Panics
///
/// When podman is missing (the message names it) or the reaper does not do
/// what it says.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_reaper_removes_a_dead_owners_leavings() {
    let images = ensure_images(Deadline(IMAGE_BUILD_DEADLINE))
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let dead = Command::new("true").spawn().and_then(|mut child| {
        let id = child.id();
        child.wait().map(|_status| id)
    });
    // The child is waited for, so /proc/<pid> is gone; a pid recycled to a
    // live process within the next few seconds would make the reaper keep the
    // orphan, which is conservative, not wrong.
    let dead = dead.expect("a process that has exited");
    let run = format!("{RUN_LABEL}=iznik-reaper-test");
    let orphan = format!("iznik-reaper-test-{}-orphan", std::process::id());
    let kept = format!("iznik-reaper-test-{}-kept", std::process::id());
    for (name, owner) in [(&orphan, dead), (&kept, std::process::id())] {
        let owner = format!("{OWNER_LABEL}={owner}");
        podman(&[
            "run",
            "--detach",
            "--rm",
            "--name",
            name,
            "--label",
            &run,
            "--label",
            &owner,
            "--timeout",
            CONTAINER_LIFETIME,
            &images.engine,
            "sleep",
            "infinity",
        ])
        .unwrap_or_else(|error| panic!("`podman run` failed: {error}"));
    }
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    let names =
        text(&podman(&["ps", "--all", "--format", "{{.Names}}"]).expect("the census answers"));
    assert!(
        !names.lines().any(|name| name == orphan),
        "the orphan survived a start"
    );
    assert!(
        names.lines().any(|name| name == kept),
        "the live owner's container was reaped"
    );
    drop(fixture);
    reap(Reap::Everything).expect("reaps everything");
    let after =
        text(&podman(&["ps", "--all", "--format", "{{.Names}}"]).expect("the census answers"));
    assert!(
        !after.lines().any(|name| name == kept),
        "reap left the live owner's container"
    );
}

/// With `sshd` deliberately not started and the cap at two seconds, `start`
/// fails with `SshNotReady` naming the alias within the cap, and tears
/// down.
///
/// # Panics
///
/// When podman is missing (the message names it), the start succeeds,
/// fails otherwise, overruns the cap by much, or leaves something behind.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_readiness_cap_fails_and_tears_down() {
    let staged = staged().unwrap_or_else(|error| panic!("staging failed: {error}"));
    let mut options = FixtureOptions::new(1, staged);
    options.host_daemon = false;
    options.ssh_ready_cap = SHORT_CAP;
    let mine = format!("iznik-{}-", std::process::id());
    let started = Instant::now();
    let outcome = Fixture::start(options);
    let elapsed = started.elapsed();
    let error = outcome.expect_err("a host without sshd is not ready");
    assert!(
        matches!(&error, FixtureError::SshNotReady { alias, .. } if alias == "host0"),
        "{error}"
    );
    assert!(
        elapsed < SHORT_CAP.saturating_add(FIXTURE_START_CEILING),
        "the failure took {elapsed:?}"
    );
    let remaining = leavings(&mine).expect("the census answers");
    assert_eq!(
        remaining,
        Vec::<String>::new(),
        "the failed start left something"
    );
}

/// `stage` is a no-op the second time, asserted by elapsed time; with an
/// override naming a directory, no build is run, asserted by a cargo that
/// does not exist.
///
/// # Panics
///
/// When staging fails, the second run is slow, or the override builds.
#[test]
#[ignore = "needs a musl toolchain; run with --run-ignored all"]
fn regression_fixture_staging_builds_nothing_the_second_time_and_honours_the_override() {
    let first = staged().unwrap_or_else(|error| panic!("staging failed: {error}"));
    let started = Instant::now();
    let second = staged().unwrap_or_else(|error| panic!("staging failed: {error}"));
    let elapsed = started.elapsed();
    assert_eq!(first, second);
    assert!(
        elapsed < RESTAGE_BOUND,
        "the second staging took {elapsed:?}"
    );
    for relative in [
        "bin/iznik-server",
        "bin/iznik-regression",
        "bin/iznik",
        "distribution/x86_64-unknown-linux-musl/iznik-server",
    ] {
        assert!(
            second.join(relative).is_file(),
            "{relative} is missing from the staged directory"
        );
    }
    let overridden = StagingOptions {
        staged: Some(PathBuf::from("/tmp/iznik-staged-override")),
        cargo: PathBuf::from("/nonexistent/cargo"),
        target_directory: PathBuf::from("/nonexistent/target"),
    };
    let chosen = stage_with(&overridden, Deadline(STAGING_DEADLINE)).expect("the override is used");
    assert_eq!(chosen, Path::new("/tmp/iznik-staged-override"));
}

/// How many processes are orphaned into the container's first process.
///
/// Enough that a container without a reaper is unmistakable, and few enough
/// that a shell makes them in an instant.
const ORPHANS: usize = 50;

/// How long they are given to be born, exit and be reaped.
const REAPING: Duration = Duration::from_secs(3);

/// Every process orphaned into a container is reaped by its first process.
///
/// A shell that backgrounds a command and exits leaves that command to the
/// container's first process. Where that process never waits — a `sleep`, as
/// it was here — the entry it leaves is permanent: nothing shows for minutes,
/// and over hours the entries accumulate until the container cannot fork and
/// every command in it fails at once. A six-hour soak found this after four
/// of them, with two thousand orphaned `ssh` control masters in the engine.
///
/// Both halves are asserted, because either alone can pass for the wrong
/// reason: that the first process is one that reaps rather than the idle
/// program, and that fifty orphans leave nothing behind.
///
/// # Panics
///
/// When podman is missing (the message names it), when the first process is
/// the idle program, or when an orphan is left in the table.
#[test]
#[ignore = "needs podman and a musl toolchain; run with --run-ignored all"]
fn regression_fixture_reaps_what_is_orphaned_into_it() {
    let fixture = started(1)
        .unwrap_or_else(|error| panic!("podman and a musl toolchain are needed: {error}"));
    for alias in [ENGINE_ALIAS, &Fixture::host_alias(0)] {
        let first = fixture
            .exec(alias, "cat /proc/1/comm", COMMAND)
            .unwrap_or_else(|error| panic!("{alias} did not say what it starts with: {error}"));
        assert_ne!(
            text(&first).trim(),
            IDLE_PROGRAM,
            "{alias} starts with something that reaps, not with the idle program"
        );
        // A shell that makes fifty background children and returns leaves
        // every one of them to the first process.
        let _orphaned = fixture
            .exec(
                alias,
                &format!("index=0; while [ $index -lt {ORPHANS} ]; do sh -c 'exit 0' & index=$((index + 1)); done; exit 0"),
                COMMAND,
            )
            .unwrap_or_else(|error| panic!("{alias} would not orphan anything: {error}"));
        thread::sleep(REAPING);
        // Counted with `awk` rather than `grep -c`, which answers a count of
        // none with a failing status and would be read here as a container
        // that would not answer.
        let left = fixture
            .exec(
                alias,
                "for held in /proc/[0-9]*; do awk '/^State:/{print $2}' \"$held/status\" \
                 2>/dev/null; done | awk '/^Z/{found++} END{print found + 0}'",
                COMMAND,
            )
            .unwrap_or_else(|error| panic!("{alias} did not count what it holds: {error}"));
        assert_eq!(
            text(&left).trim(),
            "0",
            "{alias} kept {ORPHANS} orphans it should have reaped"
        );
    }
}
