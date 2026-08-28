//! The one-round-trip probe of a host and its pure parser.
//!
//! One round trip because latency to a distant host is the dominant cost and
//! everything the bootstrap must decide can be asked at once: what the machine
//! is, whether a server is already there and which one, whether the terminfo
//! is installed and whether `tic` could install it, and where iznik may put
//! things. Six questions over one connection rather than six connections.
//!
//! The script is a constant and the reading of its answer is a pure function,
//! so the cases that decide whether a bootstrap feels good — an unwritable
//! home, an unexpected architecture, a host without `tic` — are a table of
//! strings rather than six hosts to arrange.

use core::fmt::{self, Display, Formatter};
use core::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use crate::transport::Transport;
use crate::transport::ssh::SshError;

/// The directory iznik puts things in, under whichever prefix a host allows.
pub const DIRECTORY_NAME: &str = "iznik";

/// How long the probe may take when a caller does not say.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// What a line of the probe's output separates its name from its value with.
const FIELD_SEPARATOR: char = ' ';

/// What a field says when the host has nothing to say for it.
const NOTHING: &str = "-";

/// What a yes-or-no field says for yes.
const YES: &str = "yes";

/// The one script the probe runs, exposed so a scenario can drive it directly
/// and a reader can see exactly what iznik asks a host.
///
/// It creates nothing: writability is answered by walking up to the first
/// ancestor that exists and asking about that, because a probe that made
/// directories would have changed the host before deciding whether to.
pub const PROBE_SCRIPT: &str = r#"
writable() {
  d="$1"
  while [ ! -e "$d" ] && [ "$d" != "/" ]; do d=$(dirname "$d"); done
  if [ -w "$d" ]; then printf yes; else printf no; fi
}
printf 'system %s\n' "$(uname -s)"
printf 'machine %s\n' "$(uname -m)"
if command -v tic >/dev/null 2>&1; then printf 'tic yes\n'; else printf 'tic no\n'; fi
if command -v infocmp >/dev/null 2>&1 && infocmp xterm-ghostty >/dev/null 2>&1
then printf 'terminfo yes\n'; else printf 'terminfo no\n'; fi
if [ -n "$XDG_DATA_HOME" ]; then printf 'data %s\n' "$XDG_DATA_HOME"; else printf 'data -\n'; fi
printf 'home %s\n' "$HOME"
if [ -n "$XDG_RUNTIME_DIR" ]
then printf 'runtime %s\n' "$XDG_RUNTIME_DIR"
else printf 'runtime %s\n' "${TMPDIR:-/tmp}/iznik-$(id -u)"; fi
for candidate in "${XDG_DATA_HOME:-$HOME/.local/share}/iznik" "$HOME/.local/share/iznik" \
  "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}/iznik-$(id -u)}/iznik"
do printf 'candidate %s %s\n' "$candidate" "$(writable "$candidate")"; done
said=-
for candidate in "${XDG_DATA_HOME:-$HOME/.local/share}/iznik" "$HOME/.local/share/iznik" \
  "${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}/iznik-$(id -u)}/iznik"
do
  if [ -x "$candidate/bin/iznik-server" ]
  then said=$("$candidate/bin/iznik-server" --version 2>/dev/null); break; fi
done
printf 'server %s\n' "$said"
"#;

/// The operating systems iznik has artifacts for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatingSystem {
    /// What `uname -s` calls `Linux`.
    Linux,
    /// What it calls `Darwin`.
    Darwin,
}

/// The machines iznik has artifacts for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture {
    /// What `uname -m` calls `x86_64` or `amd64`.
    X86_64,
    /// What it calls `aarch64` or `arm64`.
    Aarch64,
}

/// A server already on the host, and what it says it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledServer {
    /// Its own version.
    pub crate_version: String,
    /// The protocol it speaks.
    pub protocol_version: u16,
}

/// Everything the bootstrap needs to know about a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostProbe {
    /// What the machine runs.
    pub operating_system: OperatingSystem,
    /// What it is.
    pub architecture: Architecture,
    /// The server already there, if there is one.
    pub server: Option<InstalledServer>,
    /// Whether the terminal's terminfo is already installed.
    pub terminfo_installed: bool,
    /// Whether `tic` is there to install it.
    pub tic_available: bool,
    /// Where iznik may put things: the first candidate the host will let it
    /// write.
    pub prefix: PathBuf,
}

/// Why a host could not be probed, or could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// The command could not be run there.
    Transport {
        /// What `ssh` said.
        detail: String,
    },
    /// The host answered, and it is not one iznik has an artifact for.
    Unsupported {
        /// What it said it runs.
        operating_system: String,
        /// What it said it is.
        architecture: String,
    },
    /// The host answered something this cannot read.
    Malformed {
        /// What was missing or wrong.
        detail: String,
    },
    /// Every place iznik could put things is one this user cannot write.
    Unwritable {
        /// The places tried, in the order they were tried.
        candidates: Vec<PathBuf>,
    },
}

impl Display for ProbeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::Transport { detail } => write!(formatter, "{detail}"),
            ProbeError::Unsupported {
                operating_system,
                architecture,
            } => write!(
                formatter,
                "iznik has no server for {operating_system} on {architecture}"
            ),
            ProbeError::Malformed { detail } => {
                write!(formatter, "the host's answer could not be read: {detail}")
            }
            ProbeError::Unwritable { candidates } => write!(
                formatter,
                "none of these can be written by this user: {}",
                candidates
                    .iter()
                    .map(|held| held.display().to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            ),
        }
    }
}

impl core::error::Error for ProbeError {}

impl From<SshError> for ProbeError {
    fn from(source: SshError) -> ProbeError {
        ProbeError::Transport {
            detail: source.to_string(),
        }
    }
}

/// Something that runs one command on a host and says what it printed.
///
/// A trait rather than the transport itself, so that "the probe is one round
/// trip" is a property a test can count rather than a claim a comment makes.
pub trait RunsRemotely {
    /// Runs `command` there and gives back its standard output.
    ///
    /// # Errors
    ///
    /// [`ProbeError::Transport`] when it cannot be run or does not succeed.
    fn run(
        &self,
        command: &str,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, ProbeError>> + Send;
}

impl RunsRemotely for Transport {
    fn run(
        &self,
        command: &str,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, ProbeError>> + Send {
        let asked = command.to_owned();
        let host = self.alias();
        let spawned = match self {
            Transport::Ssh(ssh) => ssh.spawn(&[asked]),
            Transport::Local { socket } => Err(SshError::Unreachable {
                host: host.clone(),
                detail: format!(
                    "{} names a socket, and a probe needs a host to run a command on",
                    socket.display()
                ),
            }),
        };
        async move {
            let spawned = spawned?;
            let waited = tokio::time::timeout(deadline, spawned.child.wait_with_output()).await;
            let Ok(output) = waited else {
                return Err(ProbeError::from(SshError::Timeout {
                    host,
                    stage: "probing".to_owned(),
                }));
            };
            let output = output.map_err(|source| ProbeError::Transport {
                detail: format!("the probe could not be waited for: {source}"),
            })?;
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            Err(ProbeError::from(crate::transport::ssh::classify(
                &host,
                output.status.code(),
                &String::from_utf8_lossy(&output.stderr),
            )))
        }
    }
}

/// Asks a host everything at once and reads what it says.
///
/// # Errors
///
/// [`ProbeError::Transport`] when the script cannot be run,
/// [`ProbeError::Unsupported`] for a machine iznik has no artifact for,
/// [`ProbeError::Unwritable`] when nowhere is writable, and
/// [`ProbeError::Malformed`] when the answer is not one this can read.
pub async fn probe(
    transport: &impl RunsRemotely,
    deadline: Duration,
) -> Result<HostProbe, ProbeError> {
    parse(&transport.run(PROBE_SCRIPT, deadline).await?)
}

/// One field of the probe's answer: its name and the rest of its line.
fn field<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    line.strip_prefix(name)?.strip_prefix(FIELD_SEPARATOR)
}

/// The value of the first line named `name`.
fn named<'output>(output: &'output str, name: &str) -> Option<&'output str> {
    output.lines().find_map(|line| field(line.trim_end(), name))
}

/// Whether a yes-or-no field said yes.
fn said_yes(output: &str, name: &str) -> bool {
    named(output, name) == Some(YES)
}

/// The operating system a `uname -s` names, if it is one iznik serves.
fn operating_system(said: &str) -> Option<OperatingSystem> {
    match said {
        "Linux" => Some(OperatingSystem::Linux),
        "Darwin" => Some(OperatingSystem::Darwin),
        _other => None,
    }
}

/// The machine a `uname -m` names, if it is one iznik serves.
fn architecture(said: &str) -> Option<Architecture> {
    match said {
        "x86_64" | "amd64" => Some(Architecture::X86_64),
        "aarch64" | "arm64" => Some(Architecture::Aarch64),
        _other => None,
    }
}

/// The version line an installed server printed, read into what it says.
///
/// The line is `iznik-server <crate version> protocol <number>`, which is what
/// `--version` prints; anything else is a server this cannot reason about and
/// is reported as none rather than guessed at.
fn installed(said: &str) -> Option<InstalledServer> {
    let mut words = said.split_whitespace();
    let _named = words.next()?;
    let crate_version = words.next()?.to_owned();
    let _protocol = words.next().filter(|word| *word == "protocol")?;
    let protocol_version = words.next()?.parse().ok()?;
    Some(InstalledServer {
        crate_version,
        protocol_version,
    })
}

/// Every prefix the host was asked about, in order, and whether it said each
/// could be written.
fn candidates(output: &str) -> Vec<(PathBuf, bool)> {
    output
        .lines()
        .filter_map(|line| field(line.trim_end(), "candidate"))
        .filter_map(|rest| {
            let (path, answer) = rest.rsplit_once(FIELD_SEPARATOR)?;
            Some((PathBuf::from(path), answer == YES))
        })
        .collect()
}

/// Reads what a host said into what it means.
///
/// # Errors
///
/// As [`probe`], less the transport's own failures.
pub fn parse(output: &str) -> Result<HostProbe, ProbeError> {
    let missing = |what: &str| ProbeError::Malformed {
        detail: format!("no `{what}` line"),
    };
    let system = named(output, "system").ok_or_else(|| missing("system"))?;
    let machine = named(output, "machine").ok_or_else(|| missing("machine"))?;
    let (Some(operating_system), Some(architecture)) =
        (operating_system(system), architecture(machine))
    else {
        return Err(ProbeError::Unsupported {
            operating_system: system.to_owned(),
            architecture: machine.to_owned(),
        });
    };
    let offered = candidates(output);
    if offered.is_empty() {
        return Err(missing("candidate"));
    }
    let Some((prefix, _writable)) = offered.iter().find(|(_path, writable)| *writable) else {
        return Err(ProbeError::Unwritable {
            candidates: offered.into_iter().map(|(path, _writable)| path).collect(),
        });
    };
    Ok(HostProbe {
        operating_system,
        architecture,
        server: named(output, "server")
            .filter(|said| *said != NOTHING)
            .and_then(installed),
        terminfo_installed: said_yes(output, "terminfo"),
        tic_available: said_yes(output, "tic"),
        prefix: prefix.clone(),
    })
}
