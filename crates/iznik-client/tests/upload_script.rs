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

use iznik_client::bootstrap::probe::{ProbeError, parse, probe_command};
use iznik_client::bootstrap::upload::{
    BINARY_NAME, POSIX_SHELL, REMOTE_TERMINFO_SCRIPT, REMOTE_UPLOAD_SCRIPT, RemoteScript,
    hexadecimal, posix_command, remote_command,
};
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

/// How many bytes the artifact a bootstrap feeds after its script is, in the
/// case about that: far more than any shell reads ahead.
const ARTIFACT_LENGTH: usize = 200_000;

/// The places a tool this script needs is found on a host.
const TOOL_DIRECTORIES: [&str; 4] = ["/usr/bin", "/bin", "/usr/local/bin", "/usr/sbin"];

/// A directory that exists on every machine these cases run on and that the
/// user running them does not own.
const NOT_OURS: &str = "/usr";

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
        let kept = std::fs::read_to_string(held.prefix().join("bin").join("iznik-server.sha256"))?;
        assert_eq!(
            kept.trim(),
            digest_of(ARTIFACT),
            "but the digest it checked, for the probe to read rather than hash"
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
            !real.join("bin").exists(),
            "and nothing at all was made through it, not even a directory: the \
             guard runs before anything is created"
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

/// # Panics
///
/// When the script writes into a directory this user does not own.
#[test]
fn upload_script_refuses_a_prefix_this_user_does_not_own() {
    let case = || -> Result<(), Failed> {
        // The other half of the guard, and the half the `/tmp` hole was: a
        // directory that exists, that this user may not own, and that somebody
        // else could therefore have put there first. `/usr` is one on every
        // machine these cases run on.
        assert!(
            !nix::unistd::Uid::effective().is_root(),
            "these cases must not be run as root: as root every directory is \
             this user's own and the guard would have nothing to refuse"
        );
        let (held, payload) = scratch("unowned")?;
        let theirs = Path::new(NOT_OURS);
        assert!(theirs.is_dir(), "{NOT_OURS} is a directory on this machine");
        let (status, said) = refusal(run_script(theirs, &digest_of(ARTIFACT), &payload, None))?;
        assert_eq!(status, Some(REFUSED), "it refuses: {said}");
        assert!(
            said.contains("not a directory owned by this user"),
            "and says why: {said}"
        );
        assert!(
            !theirs.join("bin").join(BINARY_NAME).exists(),
            "and nothing was installed there"
        );
        drop(held);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// The login shells a host may hand iznik's command to, as `ssh` does: the
/// shell's own `-c` and the whole command as one string — C shells and
/// nushell among them, which read far less than a Bourne shell does.
const LOGIN_SHELLS: [&str; 6] = ["sh", "bash", "zsh", "fish", "csh", "tcsh"];

/// Where a login shell that is not a system one may be.
const MORE_SHELL_DIRECTORIES: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// Where a shell is on this machine, if it is.
fn shell_named(name: &str) -> Option<PathBuf> {
    located(name).ok().or_else(|| {
        MORE_SHELL_DIRECTORIES
            .iter()
            .map(|directory| Path::new(directory).join(name))
            .find(|held| held.exists())
    })
}

/// Runs what `asked` asks the way `sshd` does — `<login shell> -c <command>`,
/// with its input on standard input — under every login shell this machine
/// has, `nu` among them when it is here, and gives back what each printed.
///
/// # Errors
///
/// When `sh` is not here, or a shell that is here cannot run it.
fn as_a_login_shell(asked: &RemoteScript) -> Result<Vec<(String, String)>, Failed> {
    let (held, _payload) = scratch("login")?;
    let input = held.path.join("input");
    std::fs::write(&input, &asked.input)?;
    let mut said = Vec::new();
    for shell in LOGIN_SHELLS.iter().chain(["nu"].iter()) {
        let Some(found) = shell_named(shell) else {
            if *shell == "sh" {
                return Err("no `sh` on this machine".into());
            }
            continue;
        };
        let mut running = Command::new(found);
        running
            .arg("-c")
            .arg(&asked.command)
            .stdin(Stdio::from(File::open(&input)?));
        let done = process::run(running, Deadline(RUN_DEADLINE), Output::Capture)?;
        said.push((
            (*shell).to_owned(),
            String::from_utf8_lossy(&done.stdout).into_owned(),
        ));
    }
    drop(held);
    Ok(said)
}

/// Whether a command is one every login shell reads alike: no line break, no
/// quote, no escape, nothing a C shell or nushell reads differently from a
/// Bourne shell.
fn plain_words(command: &str) -> bool {
    command
        .chars()
        .all(|held| held.is_ascii_alphanumeric() || matches!(held, ' ' | '-' | '/' | '.' | '_'))
}

/// # Panics
///
/// When a command built for a host does not run its script under `sh` with
/// every variable exactly as it was given, whichever login shell reads it
/// first — a prefix with a quote and a space included.
#[test]
fn posix_command_runs_the_same_under_any_login_shell() {
    let case = || -> Result<(), Failed> {
        let prefix = "/tmp/it's a \"prefix\" with a `tick` and a $dollar";
        let script = "said() { printf '%s|%s' \"$IZNIK_PREFIX\" \"$IZNIK_DIGEST\"; }\nsaid\n";
        let asked = posix_command(script, &[("IZNIK_PREFIX", prefix), ("IZNIK_DIGEST", "abc")]);
        for (shell, printed) in as_a_login_shell(&asked)? {
            assert_eq!(printed, format!("{prefix}|abc"), "under {shell}: {asked:?}");
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a command a POSIX host is asked to run holds a line break, a quote or
/// anything else a C shell or nushell would read otherwise — the script and
/// its variables belong on standard input, where only `sh` reads them.
#[test]
fn posix_command_is_plain_words_for_any_login_shell() {
    let prefix = Path::new("/home/it's me/.local/share/iznik");
    for asked in [
        probe_command(),
        posix_command(REMOTE_UPLOAD_SCRIPT, &[("IZNIK_PREFIX", "/x\n'y'")]),
        remote_command(REMOTE_UPLOAD_SCRIPT, prefix, "abc", ARTIFACT.len()),
        remote_command(REMOTE_TERMINFO_SCRIPT, prefix, "abc", ARTIFACT.len()),
    ] {
        assert_eq!(asked.command, POSIX_SHELL, "{asked:?}");
        assert!(plain_words(&asked.command), "{asked:?}");
        assert!(
            asked.input.starts_with("{\n") && asked.input.contains("\nexit\n}\n"),
            "and the script is what `sh` reads, whole before any of it runs: {asked:?}"
        );
    }
    // The relay's standard input is the link, so it is the one command the
    // login shell parses: plain words, for the paths the probe offers.
    let relay = iznik_client::transport::channel::relay_command(Some(Path::new(
        "/home/me/.local/share/iznik/bin/iznik-server",
    )));
    assert!(plain_words(&relay), "{relay}");
}

/// # Panics
///
/// When the probe a host is sent does not run, under whatever login shell
/// reads it, into an answer the probe can read.
#[test]
fn probe_command_is_read_by_any_login_shell() {
    let case = || -> Result<(), Failed> {
        let asked = probe_command();
        for (shell, printed) in as_a_login_shell(&asked)? {
            let read = parse(&printed);
            assert!(
                !matches!(read, Err(ProbeError::Malformed { .. })),
                "under {shell} the probe answered: {printed}"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the upload, sent as a bootstrap sends it — the script on `sh`'s
/// standard input and the artifact after it — does not install the artifact
/// byte for byte under every `sh` this machine has, `dash` among them, which
/// reads its script a kibibyte at a time and so past where the script ends.
#[test]
fn upload_script_takes_its_payload_after_itself_under_every_sh() {
    let case = || -> Result<(), Failed> {
        // Larger than any read-ahead, so a shell that swallowed some of it
        // would be caught by the digest.
        let artifact: Vec<u8> = (0_u8..=u8::MAX).cycle().take(ARTIFACT_LENGTH).collect();
        for shell in ["sh", "dash", "bash"] {
            let Some(found) = shell_named(shell) else {
                continue;
            };
            let (held, payload) = scratch(&format!("fed-{shell}"))?;
            let asked = remote_command(
                REMOTE_UPLOAD_SCRIPT,
                &held.prefix(),
                &digest_of(&artifact),
                artifact.len(),
            );
            let mut fed = asked.input.clone().into_bytes();
            fed.extend_from_slice(&artifact);
            std::fs::write(&payload, &fed)?;
            // Through a pipe, as `ssh` gives it: a shell reading a file may
            // seek back to where its script ended, and one reading a pipe
            // cannot.
            let mut running = Command::new(located("sh")?);
            running
                .arg("-c")
                .arg("cat \"$0\" | \"$1\" -s")
                .arg(&payload)
                .arg(&found)
                .stdin(Stdio::null());
            let done = process::run(running, Deadline(RUN_DEADLINE), Output::Capture)?;
            let said = String::from_utf8_lossy(&done.stdout).into_owned();
            assert!(said.contains("installed"), "under {shell}: {said}");
            assert_eq!(
                std::fs::read(held.server())?,
                artifact,
                "under {shell}, byte for byte"
            );
            drop(held);
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Runs the probe with a prefix of the case's own as the first candidate.
///
/// # Errors
///
/// When `sh` is not here or the probe does not run.
fn probe_under(data: &Path) -> Result<String, Failed> {
    let mut command = Command::new(located("sh")?);
    command
        .arg("-c")
        .arg(iznik_client::bootstrap::probe::PROBE_SCRIPT)
        .env("XDG_DATA_HOME", data)
        .env("HOME", data)
        .stdin(Stdio::null());
    let done = process::run(command, Deadline(RUN_DEADLINE), Output::Capture)?;
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

/// # Panics
///
/// When the probe does not say the digest of an installed server — from what
/// the upload wrote beside it while that is current, and from the bytes once
/// the binary is newer than it.
#[test]
fn probe_script_says_which_bytes_a_server_is() {
    let case = || -> Result<(), Failed> {
        let (held, _payload) = scratch("digest")?;
        let bin = held.path.join("iznik").join("bin");
        std::fs::create_dir_all(&bin)?;
        // Bytes, and not a program: what is asked about is what the file
        // holds, and a case that ran a fresh executable would wait on
        // whatever a system does before it lets one run the first time.
        let server = bin.join(BINARY_NAME);
        let bytes = b"a build of the server".to_vec();
        std::fs::write(&server, &bytes)?;
        let kept = "ab".repeat(32);
        std::fs::write(bin.join("iznik-server.sha256"), format!("{kept}\n"))?;
        let now = SystemTime::now();
        let long_ago = now.checked_sub(LONG_AGO).ok_or("no clock")?;
        File::options()
            .write(true)
            .open(&server)?
            .set_times(FileTimes::new().set_modified(long_ago))?;
        let said = parse(&probe_under(&held.path)?)?;
        assert_eq!(
            said.server_digest,
            Some(kept),
            "what the upload wrote, while the binary is no newer"
        );
        File::options()
            .write(true)
            .open(bin.join("iznik-server.sha256"))?
            .set_times(FileTimes::new().set_modified(long_ago))?;
        File::options()
            .write(true)
            .open(&server)?
            .set_times(FileTimes::new().set_modified(now))?;
        let fresh = parse(&probe_under(&held.path)?)?;
        assert_eq!(
            fresh.server_digest,
            Some(digest_of(&bytes)),
            "and the bytes themselves once the binary is newer"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a probe that had to hash a server — its record missing — does not
/// write down what it found, so that every later probe hashes it again.
#[test]
fn probe_script_keeps_the_digest_it_took() {
    let case = || -> Result<(), Failed> {
        let (held, _payload) = scratch("kept")?;
        let bin = held.path.join("iznik").join("bin");
        std::fs::create_dir_all(&bin)?;
        let bytes = b"a build nobody wrote a digest for".to_vec();
        std::fs::write(bin.join(BINARY_NAME), &bytes)?;
        let said = parse(&probe_under(&held.path)?)?;
        assert_eq!(said.server_digest, Some(digest_of(&bytes)), "it hashed");
        let written = std::fs::read_to_string(bin.join("iznik-server.sha256"))?;
        assert_eq!(
            written.trim(),
            digest_of(&bytes),
            "and wrote what it found where the next probe reads it"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
