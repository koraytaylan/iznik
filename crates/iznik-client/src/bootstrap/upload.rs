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

/// The name the server is installed under.
pub const BINARY_NAME: &str = "iznik-server";

/// How many bytes a `SHA-256` is.
pub const DIGEST_BYTES: usize = 32;

/// What the host says when the digest it computed is not the one it was told.
const DIGEST_REFUSED: i32 = 65;

/// The variable the client passes the prefix in.
const PREFIX_VARIABLE: &str = "IZNIK_PREFIX";

/// The variable it passes the digest in.
const DIGEST_VARIABLE: &str = "IZNIK_DIGEST";

/// The one remote script, exposed so a scenario can drive it directly.
///
/// It takes the prefix and the digest from the environment rather than being
/// built around them, so that what runs on a host is this text and not a
/// string assembled somewhere a reader cannot see.
///
/// The name it will finally install under appears once, on the rename: a
/// dropped link leaves a `.partial-` file and never something a later run
/// would execute.
pub const REMOTE_UPLOAD_SCRIPT: &str = r#"
set -e
into="$IZNIK_PREFIX/bin"
mkdir -p "$into"
rm -f "$into"/.partial-*
partial="$into/.partial-$$"
cat > "$partial"
if command -v sha256sum >/dev/null 2>&1
then got=$(sha256sum < "$partial" | cut -d' ' -f1)
else got=$(shasum -a 256 < "$partial" | cut -d' ' -f1); fi
if [ "$got" != "$IZNIK_DIGEST" ]
then rm -f "$partial"; printf 'digest %s\n' "$got" >&2; exit 65; fi
chmod 755 "$partial"
sync
mv "$partial" "$into/iznik-server"
printf 'installed %s\n' "$into/iznik-server"
"#;

/// The remote script that compiles the terminfo iznik carries.
///
/// Separate from the upload because a host without `tic` gets the server and
/// no terminfo rather than nothing at all.
pub const REMOTE_TERMINFO_SCRIPT: &str = r#"
set -e
into="$IZNIK_PREFIX/terminfo"
mkdir -p "$into"
source="$IZNIK_PREFIX/.terminfo-source"
cat > "$source"
tic -x -o "$into" "$source"
rm -f "$source"
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
    /// Where the terminfo is, when there was a `tic` to compile it.
    pub terminfo: Option<PathBuf>,
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

    /// Whether it holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

/// The command that runs `script` on a host with the prefix and the digest it
/// needs.
#[must_use]
pub fn remote_command(script: &str, prefix: &Path, digest: &str) -> String {
    format!(
        "{PREFIX_VARIABLE}={} {DIGEST_VARIABLE}={digest} sh -c {}",
        quoted(&prefix.display().to_string()),
        quoted(script)
    )
}

/// One argument as a shell will read it back unchanged.
fn quoted(held: &str) -> String {
    format!("'{}'", held.replace('\'', r"'\''"))
}

/// Something that runs one command on a host, feeding it bytes and saying what
/// it printed.
///
/// A trait rather than the transport itself, so that what is sent and what is
/// run can be watched by a test without a host.
pub trait FeedsRemotely {
    /// Runs `command` there, writing `bytes` to its standard input in
    /// [`UPLOAD_CHUNK_LENGTH`] pieces.
    ///
    /// # Errors
    ///
    /// [`UploadError::Transport`] when it cannot be run, and
    /// [`UploadError::Refused`] when it does not succeed.
    fn feed(
        &self,
        command: &str,
        bytes: &[u8],
        stage: &'static str,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, UploadError>> + Send;
}

impl FeedsRemotely for Transport {
    fn feed(
        &self,
        command: &str,
        bytes: &[u8],
        stage: &'static str,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, UploadError>> + Send {
        let host = self.alias();
        let asked = command.to_owned();
        let payload = bytes.to_vec();
        let spawned = match self {
            Transport::Ssh(ssh) => ssh.spawn(&[asked]),
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
            if output.status.code() == Some(DIGEST_REFUSED) {
                return Err(UploadError::Digest {
                    host,
                    expected: String::new(),
                    received: said
                        .split_whitespace()
                        .next_back()
                        .unwrap_or_default()
                        .to_owned(),
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
    let bytes = std::fs::read(&artifact.path).map_err(|source| UploadError::Artifacts {
        directory: artifact.path.clone(),
        detail: source.to_string(),
    })?;
    let expected = hexadecimal(&artifact.digest);
    let command = remote_command(REMOTE_UPLOAD_SCRIPT, &probe.prefix, &expected);
    let said = transport
        .feed(&command, &bytes, "uploading the server", deadline)
        .await
        .map_err(|error| name_the_digest(error, &expected))?;
    let server =
        installed_path(&said).unwrap_or_else(|| probe.prefix.join("bin").join(BINARY_NAME));
    let terminfo = if probe.tic_available {
        let compiling = remote_command(REMOTE_TERMINFO_SCRIPT, &probe.prefix, &expected);
        transport
            .feed(
                &compiling,
                XTERM_GHOSTTY_TERMINFO.as_bytes(),
                "compiling the terminfo",
                deadline,
            )
            .await?;
        Some(probe.prefix.join("terminfo"))
    } else {
        // A host without `tic` gets the server and no terminfo; its panes are
        // told `xterm-256color`, which is the nearest lie and better than a
        // bootstrap that refuses to finish.
        None
    };
    Ok(Installed { server, terminfo })
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
