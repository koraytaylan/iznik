//! The doctor: one prerequisite per row of `CONTRIBUTING.md` section 1, each
//! reported by name with its install command when absent from the `PATH` or
//! answering wrongly, and a clean bill of health when every probe answers.
//! Every tool is a shim in a directory that is the whole `PATH` of the run,
//! so nothing here depends on this machine; the real machine is what the
//! done-when's `cargo xtask check` examines.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{Completed, Deadline, Output, ProcessError, run};
use xtask::doctor::{pinned_channel, prerequisites};
use xtask::repository_root;

/// How long a run of `xtask doctor` may take: every probe is a shim that
/// exits at once.
const RUN_DEADLINE: Duration = Duration::from_secs(20);

/// The mode of a shim: executable by everyone.
const EXECUTABLE_MODE: u32 = 0o755;

/// The prerequisites' names, in the order the contributing guide lists them,
/// with the toolchain's pinned channel where the doctor puts it.
const EXPECTED_NAMES: &[&str] = &[
    "rust toolchain {channel}",
    "cargo-nextest",
    "podman with the netavark network backend",
    "zig",
    "x86_64-linux-musl-gcc",
    "aarch64-linux-musl-gcc",
    "git",
];

/// The tools other than `cargo` the doctor probes, each shimmed to succeed
/// and print what the doctor expects.
const TOOL_SHIMS: &[(&str, &str)] = &[
    ("podman", "netavark"),
    ("zig", "0.15.2"),
    ("x86_64-linux-musl-gcc", "shim"),
    ("aarch64-linux-musl-gcc", "shim"),
    ("git", "git version shim"),
];

/// A directory of shims that is a `PATH` on its own, removed when dropped.
#[derive(Debug)]
struct Shims {
    /// The directory.
    directory: PathBuf,
}

impl Shims {
    /// Writes a shim for every probed tool, the `cargo` one answering both of
    /// the doctor's `cargo` probes.
    ///
    /// One dispatcher script and a link per tool, rather than a script per
    /// tool: macOS charges a first execution of a newly written file hundreds
    /// of milliseconds, and a run with seven of them against a twenty-second
    /// deadline is a run measuring the platform. A link to a script that has
    /// already been executed costs nothing.
    ///
    /// # Errors
    ///
    /// When a file cannot be written or linked.
    fn new(name: &str) -> Result<Shims, String> {
        let channel = pinned_channel(&repository_root()).map_err(|error| error.to_string())?;
        let directory = env::temp_dir().join(format!("iznik-doctor-{name}-{}", std::process::id()));
        fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let shims = Shims { directory };
        let tool = |output: &str| format!("    echo '{output}';;");
        let mut arms = vec![
            "  cargo)".to_owned(),
            "    case \"$1\" in".to_owned(),
            format!("      --version) echo \"cargo {channel} (shim)\";;"),
            "      nextest) echo \"cargo-nextest (shim)\";;".to_owned(),
            "      *) exit 9;;".to_owned(),
            "    esac;;".to_owned(),
        ];
        for (tool_name, output) in TOOL_SHIMS {
            arms.push(format!("  {tool_name})"));
            arms.push(tool(output));
        }
        shims.dispatch(&arms)?;
        Ok(shims)
    }

    /// Writes the dispatcher and links every tool name to it, then runs it
    /// once so the platform has executed the script before the cases do.
    ///
    /// # Errors
    ///
    /// When a file cannot be written, linked or run.
    fn dispatch(&self, arms: &[String]) -> Result<(), String> {
        let dispatcher = self.directory.join("dispatcher");
        let mut script = String::from("#!/bin/sh\ncase \"${0##*/}\" in\n");
        for arm in arms {
            script.push_str(arm);
            script.push('\n');
        }
        script.push_str("esac\n");
        fs::write(&dispatcher, script)
            .map_err(|error| format!("writing {}: {error}", dispatcher.display()))?;
        fs::set_permissions(&dispatcher, fs::Permissions::from_mode(EXECUTABLE_MODE))
            .map_err(|error| format!("chmod {}: {error}", dispatcher.display()))?;
        for (tool_name, _output) in TOOL_SHIMS {
            self.link(tool_name, &dispatcher)?;
        }
        self.link("cargo", &dispatcher)?;
        // One execution, through a link, so the first-run cost macOS charges
        // for a new file is paid here rather than inside a case's deadline.
        let ran = Command::new(self.directory.join("cargo"))
            .arg("--version")
            .output()
            .map_err(|error| format!("warming the dispatcher: {error}"))?;
        if !ran.status.success() {
            return Err(format!(
                "the dispatcher did not answer: {}",
                String::from_utf8_lossy(&ran.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Links a tool name to the dispatcher, replacing whatever was there.
    ///
    /// # Errors
    ///
    /// When the link cannot be made.
    fn link(&self, tool: &str, dispatcher: &std::path::Path) -> Result<(), String> {
        let path = self.directory.join(tool);
        let _gone = fs::remove_file(&path);
        std::os::unix::fs::symlink(dispatcher, &path)
            .map_err(|error| format!("linking {}: {error}", path.display()))
    }

    /// Replaces one shim with a script that answers differently — the
    /// platform's first-execution cost is paid once, for one file.
    ///
    /// # Errors
    ///
    /// When the link cannot be removed or the file written.
    fn replace(&self, tool: &str, script: &str) -> Result<(), String> {
        let path = self.directory.join(tool);
        let _gone = fs::remove_file(&path);
        self.write(tool, script)
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
}

impl Drop for Shims {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap_or_default();
    }
}

/// Runs `xtask doctor` with the shim directory as its whole `PATH`.
///
/// # Errors
///
/// Whatever the runner reports: a missing prerequisite is
/// [`ProcessError::Failed`].
fn run_doctor(shims: &Shims, arguments: &[&str]) -> Result<Completed, ProcessError> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .arg("doctor")
        .args(arguments)
        .env("PATH", &shims.directory);
    run(command, Deadline(RUN_DEADLINE), Output::Capture)
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

/// The prerequisites are the rows of the contributing guide, in order, and
/// the toolchain's probe expects the pinned channel.
///
/// # Panics
///
/// When the list differs.
#[test]
fn doctor_prerequisites_follow_the_contributing_guide() {
    let channel = pinned_channel(&repository_root()).expect("the pinned channel");
    let listed = prerequisites(&repository_root()).expect("the prerequisites");
    let names: Vec<String> = listed
        .iter()
        .map(|prerequisite| prerequisite.name.clone())
        .collect();
    let expected: Vec<String> = EXPECTED_NAMES
        .iter()
        .map(|name| name.replace("{channel}", &channel))
        .collect();
    assert_eq!(names, expected, "the prerequisites in order");
    let toolchain = listed.first().expect("the toolchain row");
    assert_eq!(
        toolchain.expected_output.as_deref(),
        Some(channel.as_str()),
        "the toolchain probe expects the pinned channel"
    );
    for prerequisite in &listed {
        assert!(
            !prerequisite.install.is_empty(),
            "{} has an install hint",
            prerequisite.name
        );
    }
}

/// With a `PATH` that does not have one tool, the doctor reports it by name,
/// says it is not on the `PATH`, gives its install command on standard error,
/// and exits non-zero.
///
/// # Panics
///
/// When the doctor passes or does not name the tool and its hint.
#[test]
fn doctor_reports_a_tool_absent_from_the_path_with_its_install_hint() {
    let shims = Shims::new("absent").expect("the shims");
    shims.remove("zig").expect("zig removed from the PATH");
    let (status, stderr) = failure(run_doctor(&shims, &[])).expect("a failure");
    assert_eq!(status, Some(1), "the failure status");
    assert!(
        stderr.contains("missing: zig: `zig` is not on PATH"),
        "stderr names the tool and why: {stderr}"
    );
    assert!(
        stderr.contains("install: install zig from https://ziglang.org/download/"),
        "stderr carries the install command: {stderr}"
    );
}

/// A tool that answers wrongly — podman with another network backend — is
/// reported with what it said.
///
/// # Panics
///
/// When the doctor passes or does not say what podman reported.
#[test]
fn doctor_reports_a_tool_that_answers_wrongly() {
    let shims = Shims::new("wrong").expect("the shims");
    shims
        .replace("podman", "#!/bin/sh\necho cni\n")
        .expect("a podman on another backend");
    let (status, stderr) = failure(run_doctor(&shims, &[])).expect("a failure");
    assert_eq!(status, Some(1), "the failure status");
    assert!(
        stderr.contains("reports cni rather than netavark"),
        "stderr says what podman reported: {stderr}"
    );
}

/// With every probe answering, the doctor lists every prerequisite as
/// present and exits 0.
///
/// # Panics
///
/// When the doctor fails or omits a prerequisite.
#[test]
fn doctor_passes_with_everything_present() {
    let shims = Shims::new("present").expect("the shims");
    let completed = run_doctor(&shims, &[]).expect("every prerequisite answers");
    let stdout = String::from_utf8_lossy(&completed.stdout);
    for prerequisite in prerequisites(&repository_root()).expect("the prerequisites") {
        assert!(
            stdout.contains(&format!("ok: {}", prerequisite.name)),
            "{} is listed as present: {stdout}",
            prerequisite.name
        );
    }
    assert!(completed.stderr.is_empty(), "nothing is missing");
}

/// `xtask doctor` takes no arguments.
///
/// # Panics
///
/// When an argument is not a usage error.
#[test]
fn doctor_takes_no_arguments() {
    let shims = Shims::new("arguments").expect("the shims");
    let (status, _stderr) = failure(run_doctor(&shims, &["--verbose"])).expect("a failure");
    assert_eq!(
        status,
        Some(i32::from(xtask::USAGE_EXIT_CODE)),
        "an argument is a usage error"
    );
}
