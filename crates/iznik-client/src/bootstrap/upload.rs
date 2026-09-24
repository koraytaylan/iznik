//! Uploading the server artifact and the terminfo over the same channel, with digest verification and an atomic rename.
//!
//! Over the same channel because a second connection is a second thing to get
//! right — another authentication, another firewall rule, another way for a
//! bootstrap to work on the developer's machine and not on anybody else's. The
//! bytes go down the standard input of one remote shell, and `scp` and `sftp`
//! are not required to exist.
//!
//! Verified before installed, and installed atomically. The client computes
//! the digest itself and the host checks what it received against it, so a
//! truncated upload is refused rather than executed; and the bytes land under
//! a partial name and are renamed only once they are known to be right, so a
//! link that drops in the middle leaves nothing at the name the bootstrap will
//! later run.

use core::fmt::{self, Display, Formatter};
use core::future::Future;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::bootstrap::probe::HostProbe;
use crate::bootstrap::terminfo::{TERMINAL_NAME, XTERM_GHOSTTY_TERMINFO};
use crate::transport::Transport;
use crate::transport::ssh::SshError;

/// How much of the artifact the host reads at a time. A mebibyte is large
/// enough that the reads are not the cost and small enough that a shell's
/// buffer is not the limit.
pub const UPLOAD_CHUNK_LENGTH: usize = 1024 * 1024;

/// How long an upload may take when a caller does not say. A binary of a few
/// mebibytes over an ordinary link is seconds; this is the bound at which a
/// silent link is a failure rather than a wait.
pub const UPLOAD_DEADLINE: Duration = Duration::from_mins(5);

/// The name the server is installed under on Unix.
pub const BINARY_NAME: &str = "iznik-server";

/// The name the server is installed under on `system`. Windows runs a file
/// named `iznik-server.exe`; the artifact in the bundle is still
/// [`BINARY_NAME`], and the upload renames it.
#[must_use]
pub fn executable_name(system: crate::bootstrap::probe::OperatingSystem) -> &'static str {
    match system {
        crate::bootstrap::probe::OperatingSystem::Windows => "iznik-server.exe",
        crate::bootstrap::probe::OperatingSystem::Linux
        | crate::bootstrap::probe::OperatingSystem::Darwin => BINARY_NAME,
    }
}

/// The directory under a prefix that the server goes in.
pub const BINARY_DIRECTORY: &str = "bin";

/// The directory under a prefix that the compiled terminfo goes in.
pub const TERMINFO_DIRECTORY: &str = "terminfo";

/// How many bytes a `SHA-256` is.
pub const DIGEST_BYTES: usize = 32;

/// What the host says when the digest it computed is not the one it was told.
const DIGEST_REFUSED: i32 = 65;

/// The variable the client passes the prefix in.
pub(crate) const PREFIX_VARIABLE: &str = "IZNIK_PREFIX";

/// The variable it passes the digest in.
pub(crate) const DIGEST_VARIABLE: &str = "IZNIK_DIGEST";

/// The one remote script, exposed so a scenario can drive it directly.
///
/// It takes the prefix and the digest from the environment rather than being
/// built around them, so that what runs on a host is this text and not a
/// string assembled somewhere a reader cannot see.
///
/// The name it will finally install under appears once, on the rename: a
/// dropped link leaves a `.partial-` file and never something a later run
/// would execute. The partial's own name comes from `mktemp`, and only
/// partials older than an hour are swept, because a second bootstrap of the
/// same host is a thing that happens and deleting the file it is filling would
/// break it rather than tidy up after it.
///
/// Two things it refuses. A prefix or a `bin` that is a symbolic link, or that
/// this user does not own: the probe chose the prefix from what it saw, and
/// what it saw can change before this runs — a world-writable parent lets
/// somebody else make the directory first and own what lands in it. Each is
/// checked the moment it exists and before anything under it is made, so a
/// prefix that was replaced has nothing put inside it, not even a directory. And a host
/// with no way to take a SHA-256: the digest check exists to be the one thing
/// standing between a truncated download and an executable, so it fails closed
/// rather than comparing against a value nothing computed.
///
/// It reads the artifact from its standard input: the last `IZNIK_LENGTH`
/// bytes of it when that is set, which is how a bootstrap sends it — after the
/// script itself and [`PAYLOAD_PADDING`], as [`posix_feeding`] says why — and
/// all of it when it is not, which is how a scenario that runs the script
/// directly sends it.
///
/// Once the server is in place it writes the digest it checked beside it, as
/// `iznik-server.sha256`: the probe reads that rather than hashing the whole
/// binary on every connection, and hashes only when the binary is newer than
/// what was written about it.
///
/// It flushes the file rather than the machine. `sync` writes out every
/// mounted filesystem, which on a busy host is a long wait for work that has
/// nothing to do with iznik.
pub const REMOTE_UPLOAD_SCRIPT: &str = r#"
set -e
own() {
  if [ -L "$1" ] || [ ! -d "$1" ] || [ ! -O "$1" ]
  then printf 'not a directory owned by this user: %s\n' "$1" >&2; exit 1; fi
}
prefix="$IZNIK_PREFIX"
into="$prefix/bin"
mkdir -p "$prefix"
own "$prefix"
mkdir -p "$into"
own "$into"
find "$into" -name '.partial-*' -type f -mmin +60 -exec rm -f {} + 2>/dev/null || true
partial=$(mktemp "$into/.partial-XXXXXX")
trap 'rm -f "$partial"' EXIT
if [ -n "${IZNIK_LENGTH:-}" ]
then tail -c "$IZNIK_LENGTH" > "$partial"
else cat > "$partial"; fi
if command -v sha256sum >/dev/null 2>&1
then got=$(sha256sum < "$partial" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1
then got=$(shasum -a 256 < "$partial" | cut -d' ' -f1)
else printf 'no sha256 program on this host\n' >&2; exit 1; fi
if [ -z "$got" ] || [ "$got" != "$IZNIK_DIGEST" ]
then printf 'digest %s\n' "$got" >&2; exit 65; fi
chmod 755 "$partial"
dd if=/dev/null of="$partial" conv=notrunc,fsync 2>/dev/null || true
mv "$partial" "$into/iznik-server"
trap - EXIT
printf '%s\n' "$got" > "$into/iznik-server.sha256"
printf 'installed %s\n' "$into/iznik-server"
"#;

/// The remote script that compiles the terminfo iznik carries.
///
/// Separate from the upload because a host without `tic` gets the server and
/// no terminfo rather than nothing at all.
///
/// Its source file is named by `mktemp` and removed however this ends, for the
/// same two reasons the artifact's partial is: two bootstraps of one host must
/// not write one file, and a `tic` that fails must not leave its input behind
/// in a prefix for ever.
///
/// And it refuses a prefix, or a `terminfo` under it, that is a symbolic link
/// or not this user's own, for the reason the upload does: what the probe saw
/// can change before this runs, and `tic` writing through somebody else's link
/// writes where they chose.
pub const REMOTE_TERMINFO_SCRIPT: &str = r#"
set -e
own() {
  if [ -L "$1" ] || [ ! -d "$1" ] || [ ! -O "$1" ]
  then printf 'not a directory owned by this user: %s\n' "$1" >&2; exit 1; fi
}
into="$IZNIK_PREFIX/terminfo"
mkdir -p "$IZNIK_PREFIX"
own "$IZNIK_PREFIX"
mkdir -p "$into"
own "$into"
entry=$(mktemp "$IZNIK_PREFIX/.terminfo-XXXXXX")
trap 'rm -f "$entry"' EXIT
if [ -n "${IZNIK_LENGTH:-}" ]
then tail -c "$IZNIK_LENGTH" > "$entry"
else cat > "$entry"; fi
tic -x -o "$into" "$entry"
printf 'compiled %s\n' "$into"
"#;

/// One artifact iznik can install, and the digest of its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// The triple it is for.
    pub triple: String,
    /// Where it is on this machine.
    pub path: PathBuf,
    /// The `SHA-256` of its bytes, computed on load and never read from a
    /// manifest: what is verified must be what this client will send.
    pub digest: [u8; DIGEST_BYTES],
}

/// Every artifact a distribution directory holds, by triple.
#[derive(Clone, Debug, Default)]
pub struct ArtifactSet {
    /// What was found, by the triple it is for.
    held: BTreeMap<String, Artifact>,
}

/// What an upload left on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Where the server is.
    pub server: PathBuf,
    /// Where the terminfo is, when the host has one iznik put there.
    pub terminfo: Option<PathBuf>,
    /// Why there is none, when there is none.
    ///
    /// Carried rather than logged: a pane on such a host is told
    /// `xterm-256color` instead of what it really is, and whoever notices that
    /// deserves to be able to ask why.
    pub terminfo_refused: Option<String>,
}

/// Why an upload did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// The artifacts could not be read from this machine.
    Artifacts {
        /// Where they were looked for.
        directory: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// There is no artifact for the machine the host says it is.
    NoArtifact {
        /// The triple that was wanted.
        triple: String,
        /// The triples there are.
        held: Vec<String>,
    },
    /// The command could not be run there.
    Transport {
        /// What `ssh` said.
        detail: String,
    },
    /// The host computed a different digest from the one it was told, so
    /// nothing was installed.
    Digest {
        /// The host.
        host: String,
        /// What this client computed.
        expected: String,
        /// What the host computed of what it received.
        received: String,
    },
    /// The host ran the script and it did not succeed.
    Refused {
        /// The host.
        host: String,
        /// What it was doing.
        stage: &'static str,
        /// What the host said.
        detail: String,
    },
}

impl Display for UploadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            UploadError::Artifacts { directory, detail } => {
                write!(formatter, "{}: {detail}", directory.display())
            }
            UploadError::NoArtifact { triple, held } => write!(
                formatter,
                "no artifact for {triple}; this build has {}",
                held.join(", ")
            ),
            UploadError::Transport { detail } => write!(formatter, "{detail}"),
            UploadError::Digest {
                host,
                expected,
                received,
            } => write!(
                formatter,
                "{host} received {received} where {expected} was sent, so nothing was installed"
            ),
            UploadError::Refused {
                host,
                stage,
                detail,
            } => write!(formatter, "{host}, {stage}: {detail}"),
        }
    }
}

impl core::error::Error for UploadError {}

impl From<SshError> for UploadError {
    fn from(source: SshError) -> UploadError {
        UploadError::Transport {
            detail: source.to_string(),
        }
    }
}

/// A digest as the hexadecimal a shell's `sha256sum` prints.
#[must_use]
pub fn hexadecimal(digest: &[u8; DIGEST_BYTES]) -> String {
    use core::fmt::Write as _;
    digest.iter().fold(String::new(), |mut held, byte| {
        // A digit that will not format is not a thing that happens.
        let _written = write!(held, "{byte:02x}");
        held
    })
}

impl ArtifactSet {
    /// Every artifact under `directory`, laid out as `xtask distribution` and
    /// the staging tree lay them out: one `<triple>/iznik-server` per triple.
    ///
    /// The digest of each is computed from the file's own bytes. No manifest
    /// is read: what a bootstrap verifies must be what it is about to send,
    /// and a manifest is a second copy of that with its own way of being
    /// wrong.
    ///
    /// # Errors
    ///
    /// [`UploadError::Artifacts`] when the directory cannot be read.
    pub fn load(directory: &Path) -> Result<ArtifactSet, UploadError> {
        use sha2::Digest as _;
        let complain = |detail: String| UploadError::Artifacts {
            directory: directory.to_path_buf(),
            detail,
        };
        let listed = std::fs::read_dir(directory).map_err(|source| complain(source.to_string()))?;
        let mut held = BTreeMap::new();
        for entry in listed {
            let entry = entry.map_err(|source| complain(source.to_string()))?;
            let path = entry.path().join(BINARY_NAME);
            let Some(triple) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let digest = sha2::Sha256::digest(&bytes).into();
            held.insert(
                triple.clone(),
                Artifact {
                    triple,
                    path,
                    digest,
                },
            );
        }
        Ok(ArtifactSet { held })
    }

    /// The artifact for `triple`.
    ///
    /// # Errors
    ///
    /// [`UploadError::NoArtifact`] naming the triple wanted and the ones there
    /// are, because a bootstrap that says only "no artifact" leaves a person
    /// guessing which build they have.
    pub fn for_triple(&self, triple: &str) -> Result<&Artifact, UploadError> {
        self.held
            .get(triple)
            .ok_or_else(|| UploadError::NoArtifact {
                triple: triple.to_owned(),
                held: self.held.keys().cloned().collect(),
            })
    }

    /// The triples this holds.
    #[must_use]
    pub fn triples(&self) -> Vec<&str> {
        self.held.keys().map(String::as_str).collect()
    }

    /// Whether it holds an artifact whose bytes have this digest: whether a
    /// server that says it was built from those bytes is one of this build's.
    #[must_use]
    pub fn carries(&self, digest: &[u8; DIGEST_BYTES]) -> bool {
        self.held
            .values()
            .any(|artifact| artifact.digest == *digest)
    }

    /// Whether it holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

/// The command that runs `script` on a host with the prefix and the digest it
/// needs, and `length` bytes of payload fed after it.
#[must_use]
pub fn remote_command(script: &str, prefix: &Path, digest: &str, length: usize) -> RemoteScript {
    posix_feeding(
        script,
        &[
            (PREFIX_VARIABLE, &prefix.display().to_string()),
            (DIGEST_VARIABLE, digest),
        ],
        length,
    )
}

/// The one command a POSIX script is run with, whatever the host's login
/// shell: `sh`, reading the script from its standard input.
///
/// Two plain words, and so a command every login shell reads the same — a
/// Bourne shell, fish, nushell, and a C shell, which cannot read a quoted
/// argument that spans lines and so could never run `sh -c '<script>'`.
pub const POSIX_SHELL: &str = "sh -s";

/// How many newlines stand between a script and the payload fed after it.
///
/// A shell reading its script from a pipe may read ahead of the command it is
/// running — `dash`, the `sh` of Debian and Ubuntu, reads a kibibyte at a
/// time — and whatever it read is gone from the pipe before the script's own
/// reader starts. So the payload is not put straight after the script: this
/// many newlines are, far more than any shell reads ahead, and the script
/// takes the payload as the last `IZNIK_LENGTH` bytes of what is left.
pub const PAYLOAD_PADDING: usize = 64 * 1024;

/// The variable the length of a payload is passed in.
pub(crate) const LENGTH_VARIABLE: &str = "IZNIK_LENGTH";

/// What a host is asked to run: a command its login shell reads, and the
/// text written to that command's standard input before anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteScript {
    /// The command, as `ssh` hands it to the login shell.
    pub command: String,
    /// What goes on its standard input first: for a POSIX script, the script.
    pub input: String,
}

impl RemoteScript {
    /// A command that is the whole of what is asked, reading nothing it is
    /// not fed — a Windows host's.
    #[must_use]
    pub fn alone(command: String) -> RemoteScript {
        RemoteScript {
            command,
            input: String::new(),
        }
    }
}

/// A POSIX script as a host is asked to run it: [`POSIX_SHELL`], with the
/// script on its standard input and every variable it reads assigned and
/// exported at its top, in POSIX quoting that only that `sh` reads.
///
/// The script is one brace group, ending in `exit`, so that `sh` has read the
/// whole of it before it runs any of it — a payload fed after it is never
/// taken for more script — and stops without reading further.
#[must_use]
pub fn posix_command(script: &str, variables: &[(&str, &str)]) -> RemoteScript {
    use core::fmt::Write as _;
    let mut whole = String::from("{\n");
    for (name, value) in variables {
        // Writing to a `String` cannot fail.
        let _written = writeln!(whole, "{name}={}\nexport {name}", quoted(value));
    }
    whole.push_str(script);
    whole.push_str("\nexit\n}\n");
    RemoteScript {
        command: POSIX_SHELL.to_owned(),
        input: whole,
    }
}

/// A POSIX script that reads a payload of `length` bytes fed after it, as
/// [`posix_command`] makes one, with the length in `IZNIK_LENGTH` and
/// [`PAYLOAD_PADDING`] after it.
#[must_use]
pub fn posix_feeding(script: &str, variables: &[(&str, &str)], length: usize) -> RemoteScript {
    let counted = length.to_string();
    let mut every: Vec<(&str, &str)> = variables.to_vec();
    every.push((LENGTH_VARIABLE, &counted));
    let mut asked = posix_command(script, &every);
    asked.input.push_str(&"\n".repeat(PAYLOAD_PADDING));
    asked
}

/// One argument as a shell will read it back unchanged.
pub(crate) fn quoted(held: &str) -> String {
    format!("'{}'", held.replace('\'', r"'\''"))
}

/// Something that runs one command on a host, feeding it bytes and saying what
/// it printed.
///
/// A trait rather than the transport itself, so that what is sent and what is
/// run can be watched by a test without a host.
pub trait FeedsRemotely {
    /// Runs `asked` there, writing its input and then `bytes` to its standard
    /// input, the bytes in [`UPLOAD_CHUNK_LENGTH`] pieces.
    ///
    /// # Errors
    ///
    /// [`UploadError::Transport`] when it cannot be run, and
    /// [`UploadError::Refused`] when it does not succeed.
    fn feed(
        &self,
        asked: &RemoteScript,
        bytes: &[u8],
        stage: &'static str,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, UploadError>> + Send;
}

impl FeedsRemotely for Transport {
    fn feed(
        &self,
        asked: &RemoteScript,
        bytes: &[u8],
        stage: &'static str,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, UploadError>> + Send {
        let host = self.alias();
        let script = asked.input.clone().into_bytes();
        let payload = bytes.to_vec();
        let spawned = match self {
            Transport::Ssh(ssh) => ssh.spawn(core::slice::from_ref(&asked.command)),
            Transport::Local { socket } => Err(SshError::Unreachable {
                host: host.clone(),
                detail: format!(
                    "{} names a socket, and an upload needs a host to run a script on",
                    socket.display()
                ),
            }),
        };
        async move {
            let mut spawned = spawned?;
            let writing = spawned.child.stdin.take();
            let feeding = async move {
                use tokio::io::AsyncWriteExt as _;
                let Some(mut writing) = writing else {
                    return;
                };
                if writing.write_all(&script).await.is_err() {
                    return;
                }
                for piece in payload.chunks(UPLOAD_CHUNK_LENGTH) {
                    if writing.write_all(piece).await.is_err() {
                        return;
                    }
                }
                // The host reads until its input ends, so this is what tells it
                // the artifact is whole.
                let _closed = writing.shutdown().await;
            };
            let ended = spawned.child.wait_with_output();
            let (_fed, waited) = tokio::join!(feeding, tokio::time::timeout(deadline, ended));
            let Ok(output) = waited else {
                return Err(UploadError::from(SshError::Timeout {
                    host,
                    stage: stage.to_owned(),
                }));
            };
            let output = output.map_err(|source| UploadError::Transport {
                detail: format!("the upload could not be waited for: {source}"),
            })?;
            let said = String::from_utf8_lossy(&output.stderr).into_owned();
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            // A digest refusal is the one exit that carries a digest, and it
            // says so on its own line. Reading the last word of whatever came
            // back would turn a `mv` that happened to fail with the same code
            // — or a login shell writing to standard error — into "the host
            // received <garbage>", which is worse than saying nothing.
            if output.status.code() == Some(DIGEST_REFUSED)
                && let Some(received) = reported_digest(&said)
            {
                return Err(UploadError::Digest {
                    host,
                    expected: String::new(),
                    received: received.to_owned(),
                });
            }
            Err(UploadError::Refused {
                host,
                stage,
                detail: said.trim().to_owned(),
            })
        }
    }
}

/// Puts the server on a host, and the terminfo beside it when the host has a
/// `tic` to compile it.
///
/// # Errors
///
/// [`UploadError::Artifacts`] when the artifact cannot be read here,
/// [`UploadError::Digest`] when the host received something else,
/// [`UploadError::Refused`] when the host's script failed, and
/// [`UploadError::Transport`] when it could not be run at all.
pub async fn upload(
    transport: &impl FeedsRemotely,
    artifact: &Artifact,
    probe: &HostProbe,
    deadline: Duration,
) -> Result<Installed, UploadError> {
    let bytes = tokio::fs::read(&artifact.path)
        .await
        .map_err(|source| UploadError::Artifacts {
            directory: artifact.path.clone(),
            detail: source.to_string(),
        })?;
    unchanged(artifact, &bytes)?;
    let expected = hexadecimal(&artifact.digest);
    let command = match probe.operating_system {
        crate::bootstrap::probe::OperatingSystem::Windows => RemoteScript::alone(
            crate::bootstrap::windows::upload_command(&probe.prefix, &expected),
        ),
        crate::bootstrap::probe::OperatingSystem::Linux
        | crate::bootstrap::probe::OperatingSystem::Darwin => {
            remote_command(REMOTE_UPLOAD_SCRIPT, &probe.prefix, &expected, bytes.len())
        }
    };
    let said = transport
        .feed(&command, &bytes, "uploading the server", deadline)
        .await
        .map_err(|error| name_the_digest(error, &expected))?;
    let server = installed_path(&said).unwrap_or_else(|| {
        probe
            .prefix
            .join(BINARY_DIRECTORY)
            .join(executable_name(probe.operating_system))
    });
    let (terminfo, terminfo_refused) = terminfo_for(transport, probe, &expected, deadline).await;
    Ok(Installed {
        server,
        terminfo,
        terminfo_refused,
    })
}

/// Refuses an artifact whose bytes are not the ones its digest was taken over.
///
/// The digest is computed when the set is loaded and the bytes are read again
/// here, so between the two the file may have been replaced. What the host is
/// told to expect must be a digest of what it is about to be sent, or the
/// check on the far end compares one file against another.
///
/// # Errors
///
/// [`UploadError::Artifacts`] naming both digests.
fn unchanged(artifact: &Artifact, bytes: &[u8]) -> Result<(), UploadError> {
    use sha2::Digest as _;
    let read: [u8; DIGEST_BYTES] = sha2::Sha256::digest(bytes).into();
    if read == artifact.digest {
        return Ok(());
    }
    Err(UploadError::Artifacts {
        directory: artifact.path.clone(),
        detail: format!(
            "changed since it was loaded: {} was read where {} was expected",
            hexadecimal(&read),
            hexadecimal(&artifact.digest)
        ),
    })
}

/// The terminfo the host ends up with, and why it is not this build's when it
/// is not.
///
/// A `tic` that fails is not a bootstrap that fails. By the time this runs the
/// server is on the host and works; what a missing terminfo costs is a pane
/// told `xterm-256color` instead of what it really is, and that is a far
/// smaller thing than a connection that refuses to finish. The one thing that
/// saves such a host is an entry the probe already found, which is iznik's own
/// from an earlier run: a host that cannot compile one today may still have
/// one from a day it could.
async fn terminfo_for(
    transport: &impl FeedsRemotely,
    probe: &HostProbe,
    expected: &str,
    deadline: Duration,
) -> (Option<PathBuf>, Option<String>) {
    let already = probe.prefix.join(TERMINFO_DIRECTORY);
    let without = |reason: String| {
        if probe.terminfo_installed {
            (Some(already.clone()), None)
        } else {
            (None, Some(reason))
        }
    };
    if !probe.tic_available {
        return without("the host has no `tic` to compile one with".to_owned());
    }
    let compiling = remote_command(
        REMOTE_TERMINFO_SCRIPT,
        &probe.prefix,
        expected,
        XTERM_GHOSTTY_TERMINFO.len(),
    );
    match transport
        .feed(
            &compiling,
            XTERM_GHOSTTY_TERMINFO.as_bytes(),
            "compiling the terminfo",
            deadline,
        )
        .await
    {
        Ok(_said) => (Some(already), None),
        Err(refusal) => without(refusal.to_string()),
    }
}

/// The digest a host said it computed, from the one line that says so.
fn reported_digest(said: &str) -> Option<&str> {
    said.lines()
        .find_map(|line| line.trim().strip_prefix("digest "))
}

/// Puts the digest this client sent into a refusal that only knows what the
/// host received.
fn name_the_digest(error: UploadError, expected: &str) -> UploadError {
    match error {
        UploadError::Digest { host, received, .. } => UploadError::Digest {
            host,
            expected: expected.to_owned(),
            received,
        },
        other => other,
    }
}

/// Where the host said it installed the server.
fn installed_path(said: &str) -> Option<PathBuf> {
    said.lines()
        .find_map(|line| line.trim().strip_prefix("installed "))
        .map(PathBuf::from)
}

/// The terminal a pane is told it is, when the terminfo went with the server.
#[must_use]
pub fn terminal_name() -> &'static str {
    TERMINAL_NAME
}
