//! The script a host runs, run here.
//!
//! `REMOTE_UPLOAD_SCRIPT` is the one part of a bootstrap that executes on
//! somebody else's machine, and what matters about it is not its text but what
//! `sh` does with it: install only what matches the digest, fail closed when
//! nothing can compute one, refuse a prefix that is not this user's own, and
//! leave nothing behind either way. Those are properties of the constant and
//! of a shell, and neither needs a network — so they are proven against the
//! constant itself here, rather than against a shortened copy of it retyped in
//! a scenario, where changing the real one would leave the copy green.

use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use iznik_client::bootstrap::upload::{BINARY_NAME, REMOTE_UPLOAD_SCRIPT, hexadecimal};
use iznik_harness::process::{self, Completed, Deadline, Output, ProcessError};

/// How long one run of the script may take. It is milliseconds; this is a
/// bound, not an allowance.
const RUN_DEADLINE: Duration = Duration::from_secs(30);

/// What the host says when the digest it computed is not the one it was told.
const DIGEST_REFUSED: i32 = 65;

/// What it says when it will not write where it was told.
const REFUSED: i32 = 1;

/// The bytes these cases install, which are a program so that the executable
/// bit means something.
const ARTIFACT: &[u8] = b"#!/bin/sh\nexit 0\n";

/// The places a tool this script needs is found on a host.
const TOOL_DIRECTORIES: [&str; 4] = ["/usr/bin", "/bin", "/usr/local/bin", "/usr/sbin"];

/// Everything the script needs beyond a way to take a `SHA-256`.
const TOOLS: [&str; 8] = ["cat", "chmod", "mkdir", "mv", "mktemp", "find", "dd", "rm"];

/// How old a partial must be before the script sweeps it, with room either
/// side of the hour the script asks for.
const LONG_AGO: Duration = Duration::from_hours(2);

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

impl Scratch {
    /// The prefix a run of the script installs under.
    fn prefix(&self) -> PathBuf {
        self.path.join("prefix")
    }

    /// Where the server ends up under that prefix.
    fn server(&self) -> PathBuf {
        self.prefix().join("bin").join(BINARY_NAME)
    }
}

/// A scratch directory named for `case`, with the bytes to be uploaded in it.
///
/// # Errors
///
/// When it cannot be made or written.
fn scratch(case: &str) -> Result<(Scratch, PathBuf), Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-script-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    let payload = path.join("payload");
    std::fs::write(&payload, ARTIFACT)?;
    Ok((Scratch { path }, payload))
}

/// Where a tool is on this machine.
///
/// # Errors
///
/// When it is nowhere this looks, which is a machine these cases cannot run
/// on rather than a failure of the script.
fn located(name: &str) -> Result<PathBuf, Failed> {
    for directory in TOOL_DIRECTORIES {
        let held = Path::new(directory).join(name);
        if held.exists() {
            return Ok(held);
        }
    }
    Err(format!("no `{name}` in {TOOL_DIRECTORIES:?}").into())
}

/// The digest of some bytes, as the script is told it.
fn digest_of(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let held: [u8; 32] = sha2::Sha256::digest(bytes).into();
    hexadecimal(&held)
}

/// Runs the script with `prefix` and `digest`, feeding it `payload`.
///
/// `path` narrows what the script may find, for the case about a host with no
/// way to take a digest.
///
/// # Errors
///
/// [`ProcessError`] as the run gives it, so a case can look at the status the
/// script exited with.
fn run_script(
    prefix: &Path,
    digest: &str,
    payload: &Path,
    path: Option<&Path>,
) -> Result<Completed, Failed> {
    let shell = located("sh")?;
    let mut command = Command::new(shell);
    command
        .arg("-c")
        .arg(REMOTE_UPLOAD_SCRIPT)
        .env("IZNIK_PREFIX", prefix)
        .env("IZNIK_DIGEST", digest)
        .stdin(Stdio::from(File::open(payload)?));
    if let Some(narrowed) = path {
        command.env("PATH", narrowed);
    }
    Ok(process::run(
        command,
        Deadline(RUN_DEADLINE),
        Output::Capture,
    )?)
}

/// Every `.partial-` file beside the server, in name order.
///
/// # Errors
///
/// When the directory cannot be read.
fn partials(directory: &Path) -> Result<Vec<String>, Failed> {
    let mut held = Vec::new();
    let Ok(listed) = std::fs::read_dir(directory) else {
        return Ok(held);
    };
    for entry in listed {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with(".partial-") {
            held.push(name);
        }
    }
    held.sort();
    Ok(held)
}

/// The status and standard error of a run that was meant to fail.
///
/// # Errors
///
/// When it succeeded, or failed some other way than by exiting.
fn refusal(run: Result<Completed, Failed>) -> Result<(Option<i32>, String), Failed> {
    match run {
        Ok(done) => Err(format!(
            "the script succeeded where it should have refused: {}",
            String::from_utf8_lossy(&done.stdout)
        )
        .into()),
        Err(error) => match error.downcast::<ProcessError>() {
            Ok(held) => match *held {
                ProcessError::Failed {
                    status,
                    stderr_tail,
                    ..
                } => Ok((status.code(), stderr_tail)),
                other => Err(Box::new(other).into()),
            },
            Err(other) => Err(other),
        },
    }
}

/// # Panics
///
/// When the script does not install what its digest names, byte for byte and
/// executable, with nothing left beside it.
#[test]
fn upload_script_installs_what_matches_its_digest() {
    let case = || -> Result<(), Failed> {
        let (held, payload) = scratch("installs")?;
        let done = run_script(&held.prefix(), &digest_of(ARTIFACT), &payload, None)?;
        let said = String::from_utf8_lossy(&done.stdout).into_owned();
        let server = held.server();
        assert!(
            said.contains(&format!("installed {}", server.display())),
            "it says where it put it: {said}"
        );
        assert_eq!(std::fs::read(&server)?, ARTIFACT, "byte for byte");
        let mode = std::fs::metadata(&server)?.permissions().mode();
        assert!(mode & 0o111 != 0, "and executable: {mode:o}");
        assert_eq!(
            partials(&held.prefix().join("bin"))?,
            Vec::<String>::new(),
            "and nothing of its own left beside it"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When bytes whose digest is not the one the script was told are installed,
/// or the refusal does not say what the host computed.
#[test]
fn upload_script_refuses_what_it_did_not_send() {
    let case = || -> Result<(), Failed> {
        let (held, payload) = scratch("refuses")?;
        let wrong = digest_of(b"not what is in the file");
        let (status, said) = refusal(run_script(&held.prefix(), &wrong, &payload, None))?;
        assert_eq!(status, Some(DIGEST_REFUSED), "the digest exit: {said}");
        assert!(
            said.contains(&format!("digest {}", digest_of(ARTIFACT))),
            "and it says what it computed, so the two can be compared: {said}"
        );
        assert!(
            !held.server().exists(),
            "and nothing is at the name it would have been installed under"
        );
        assert_eq!(
            partials(&held.prefix().join("bin"))?,
            Vec::<String>::new(),
            "and the partial it wrote is gone"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the script sweeps a partial a second bootstrap of the same host is
/// still filling, or keeps one nothing is filling any more.
#[test]
fn upload_script_sweeps_only_a_partial_nothing_is_filling() {
    let case = || -> Result<(), Failed> {
        let (held, payload) = scratch("partials")?;
        let into = held.prefix().join("bin");
        std::fs::create_dir_all(&into)?;
        let filling = into.join(".partial-filling");
        let dropped = into.join(".partial-dropped");
        std::fs::write(&filling, b"half an artifact")?;
        std::fs::write(&dropped, b"half an artifact")?;
        let long_ago = SystemTime::now()
            .checked_sub(LONG_AGO)
            .ok_or("this machine's clock is before the epoch")?;
        File::options()
            .write(true)
            .open(&dropped)?
            .set_times(FileTimes::new().set_modified(long_ago))?;
        let _done = run_script(&held.prefix(), &digest_of(ARTIFACT), &payload, None)?;
        assert!(
            filling.exists(),
            "the one another run is filling is still there"
        );
        assert!(!dropped.exists(), "and the one from a dropped link is not");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the script writes into a prefix that is not a directory this user
/// owns.
#[test]
fn upload_script_refuses_a_prefix_that_is_not_this_user_own() {
    let case = || -> Result<(), Failed> {
        let (held, payload) = scratch("elsewhere")?;
        // A symbolic link is the shape of the hazard this closes: the probe
        // chose a prefix under a directory anybody may write, and between the
        // probe and this somebody pointed it somewhere of their choosing.
        let real = held.path.join("real");
        std::fs::create_dir_all(&real)?;
        let linked = held.path.join("linked");
        std::os::unix::fs::symlink(&real, &linked)?;
        let (status, said) = refusal(run_script(&linked, &digest_of(ARTIFACT), &payload, None))?;
        assert_eq!(status, Some(REFUSED), "it refuses: {said}");
        assert!(
            said.contains("not a directory owned by this user"),
            "and says why: {said}"
        );
        assert!(
            !real.join("bin").join(BINARY_NAME).exists(),
            "and nothing was installed through the link"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a host with no way to take a `SHA-256` installs the artifact anyway.
#[test]
fn upload_script_fails_closed_with_no_way_to_take_a_digest() {
    let case = || -> Result<(), Failed> {
        let (held, payload) = scratch("closed")?;
        // Everything the script needs except a digest program: the check
        // exists to be the one thing between a truncated transfer and an
        // executable, so a host that cannot do it must be told, not trusted.
        let tools = held.path.join("tools");
        std::fs::create_dir_all(&tools)?;
        for name in TOOLS {
            std::os::unix::fs::symlink(located(name)?, tools.join(name))?;
        }
        let (status, said) = refusal(run_script(
            &held.prefix(),
            &digest_of(ARTIFACT),
            &payload,
            Some(&tools),
        ))?;
        assert_eq!(status, Some(REFUSED), "it refuses: {said}");
        assert!(
            said.contains("no sha256 program"),
            "and says what the host is missing: {said}"
        );
        assert!(
            !held.server().exists(),
            "and installs nothing it could not check"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
