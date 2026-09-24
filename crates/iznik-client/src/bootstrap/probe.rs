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
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::bootstrap::upload::{RemoteScript, posix_command};
use crate::transport::Transport;
use crate::transport::ssh::SshError;

/// How long the probe may take when a caller does not say.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// What a line of the probe's output separates its name from its value with.
const FIELD_SEPARATOR: char = ' ';

/// What a field says when the host has nothing to say for it.
const NOTHING: &str = "-";

/// What a yes-or-no field says for yes.
const YES: &str = "yes";

/// What every version line an iznik server prints begins with.
const SERVER_NAME: &str = "iznik-server";

/// The word a version line puts before the protocol it speaks.
const PROTOCOL_WORD: &str = "protocol";

/// The one script the probe runs, exposed so a scenario can drive it directly
/// and a reader can see exactly what iznik asks a host.
///
/// Three things about it are deliberate.
///
/// It creates nothing: writability is answered by walking up to the first
/// ancestor that exists, because a probe that made directories would have
/// changed the host before deciding whether to. A candidate that exists and
/// belongs to somebody else is not writable however its parent is set —
/// `/tmp` is world-writable, and without that rule another local user could
/// leave a directory where iznik would later install and run a binary.
///
/// Every variable is read with a default, because a host whose `~/.bashrc`
/// sets `-u` would otherwise turn every probe into a failed command.
///
/// And each candidate answers for itself about the server installed *there*,
/// rather than one loop finding a binary and another choosing a prefix: what
/// the bootstrap will run is `<prefix>/bin/iznik-server`, so what it must know
/// is whether one is at the prefix it settled on. Only the first line of a
/// version is taken, so a server that says more cannot append fields to this
/// answer.
///
/// What says *which build* a server is, is its bytes: every build of one
/// version answers `--version` alike. So each candidate also says the SHA-256
/// of the server there — read from the `iznik-server.sha256` the upload wrote
/// beside it when that is no older than the binary, so a reconnection does
/// not hash a binary it has hashed before, and computed otherwise — and then
/// written there, so that a server whose record is missing or stale is hashed
/// once and not on every probe. A record that cannot be written is no failure:
/// the next probe hashes again.
pub const PROBE_SCRIPT: &str = r#"
writable() {
  if [ -e "$1" ]; then
    if [ -w "$1" ] && [ -O "$1" ]; then printf yes; else printf no; fi
    return
  fi
  d=$(dirname "$1")
  while [ ! -e "$d" ] && [ "$d" != "/" ]; do d=$(dirname "$d"); done
  if [ -w "$d" ]; then printf yes; else printf no; fi
}
home=${HOME:-/}
runtime=${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}/iznik-$(id -u)}
data=${XDG_DATA_HOME:-$home/.local/share}
printf 'system %s
' "$(uname -s)"
printf 'machine %s
' "$(uname -m)"
if command -v tic >/dev/null 2>&1; then printf 'tic yes
'; else printf 'tic no
'; fi
index=0
for candidate in "$data/iznik" "$home/.local/share/iznik" "$runtime"
do
  said=-
  digest=-
  server="$candidate/bin/iznik-server"
  kept="$server.sha256"
  if [ -x "$server" ] && [ -O "$server" ]
  then said=$("$server" --version 2>/dev/null | head -n 1); fi
  if [ -f "$server" ] && [ -O "$server" ]
  then
    if [ -f "$kept" ] && [ -O "$kept" ] && [ ! "$server" -nt "$kept" ]
    then read -r digest < "$kept" || digest=-
    elif command -v sha256sum >/dev/null 2>&1
    then digest=$(sha256sum < "$server" | cut -d' ' -f1)
    elif command -v shasum >/dev/null 2>&1
    then digest=$(shasum -a 256 < "$server" | cut -d' ' -f1); fi
    if [ -n "$digest" ] && [ "$digest" != - ] && { [ ! -f "$kept" ] || [ "$server" -nt "$kept" ]; }
    then { printf '%s\n' "$digest" > "$kept.$$" && mv -f "$kept.$$" "$kept"; } 2>/dev/null || rm -f "$kept.$$" 2>/dev/null
    fi
  fi
  entry=no
  for compiled in "$candidate"/terminfo/*/xterm-ghostty
  do if [ -r "$compiled" ]; then entry=yes; fi; done
  printf 'candidate %s writable %s
' "$index" "$(writable "$candidate")"
  printf 'candidate %s version %s
' "$index" "$said"
  printf 'candidate %s terminfo %s
' "$index" "$entry"
  printf 'candidate %s digest %s
' "$index" "${digest:--}"
  printf 'candidate %s path %s
' "$index" "$candidate"
  index=$((index + 1))
done
"#;

/// What `uname -s` begins with on the POSIX layers Windows can carry — Git
/// for Windows, MSYS2, Cygwin. A host whose `ssh` login found one of those
/// is a Windows host all the same, and is asked again as one.
const WINDOWS_POSIX_LAYERS: &[&str] = &["MINGW", "MSYS", "CYGWIN"];

/// What the probe asks a POSIX host to run: [`PROBE_SCRIPT`], read by `sh`
/// from its standard input, whatever the person's login shell is.
#[must_use]
pub fn probe_command() -> RemoteScript {
    posix_command(PROBE_SCRIPT, &[])
}

/// The operating systems iznik has artifacts for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatingSystem {
    /// What `uname -s` calls `Linux`.
    Linux,
    /// What it calls `Darwin`.
    Darwin,
    /// What Windows reports as `Windows_NT`. OpenSSH there is an optional
    /// feature, off until someone turns it on.
    Windows,
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
    /// The SHA-256 of that server's bytes, in lowercase hexadecimal, when the
    /// host could say: what tells two builds of one version apart.
    pub server_digest: Option<String>,
    /// Whether the terminfo iznik carries is already under one of the
    /// candidate prefixes.
    ///
    /// Asked as a file under a prefix rather than through `infocmp`, because
    /// `TERMINFO_DIRS` is where ncurses looks *first* and not where it looks
    /// *only*: a host whose own ncurses ships an `xterm-ghostty` would answer
    /// yes with nothing of iznik's anywhere, and an upload that read the
    /// answer would report a directory that does not exist.
    pub terminfo_installed: bool,
    /// Whether `tic` is there to install it.
    pub tic_available: bool,
    /// Where iznik may put things: the first candidate the host will let it
    /// write.
    pub prefix: PathBuf,
    /// Where a server of iznik's already is, when one is anywhere: the first
    /// candidate holding one. Usually `prefix`, and not always — a candidate
    /// that was writable when the server was installed may not be now — and
    /// it is where taking iznik off must look.
    pub installed_at: Option<PathBuf>,
}

/// Why a host could not be probed, or could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// `ssh` could not run the command there, or ran it and it failed —
    /// kept as `ssh`'s own classification, because whether a person must act
    /// before trying again (a refused key, a host key that changed) is
    /// decided from it.
    Ssh(SshError),
    /// The command could not be run there, for a reason that is not `ssh`'s.
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
            ProbeError::Ssh(source) => write!(formatter, "{source}"),
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
                "none of these is a directory this user both owns and can write: {}",
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
        ProbeError::Ssh(source)
    }
}

impl ProbeError {
    /// Whether the command reached the host's shell and failed there, which
    /// is the one failure that says the shell may not be a POSIX one.
    #[must_use]
    pub fn ran_and_failed(&self) -> bool {
        matches!(self, ProbeError::Ssh(SshError::RemoteCommandFailed { .. }))
    }
}

/// Something that runs one command on a host and says what it printed.
///
/// A trait rather than the transport itself, so that "the probe is one round
/// trip" is a property a test can count rather than a claim a comment makes.
pub trait RunsRemotely {
    /// Runs `asked` there — its command, with its input on the command's
    /// standard input — and gives back its standard output.
    ///
    /// # Errors
    ///
    /// [`ProbeError::Ssh`] when `ssh` cannot run it or it does not succeed,
    /// and [`ProbeError::Transport`] when it cannot be waited for.
    fn run(
        &self,
        asked: &RemoteScript,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, ProbeError>> + Send;
}

impl RunsRemotely for Transport {
    fn run(
        &self,
        asked: &RemoteScript,
        deadline: Duration,
    ) -> impl Future<Output = Result<String, ProbeError>> + Send {
        self.run_for(asked, deadline, "probing")
    }
}

impl Transport {
    /// Runs `asked` on the host and gives back its standard output, naming
    /// what it was for — `doing` — in the refusal a deadline becomes, so a
    /// daemon that would not stop is not reported as a host that would not
    /// answer a probe.
    ///
    /// # Errors
    ///
    /// As [`RunsRemotely::run`].
    pub fn run_for(
        &self,
        asked: &RemoteScript,
        deadline: Duration,
        doing: &'static str,
    ) -> impl Future<Output = Result<String, ProbeError>> + Send {
        let input = asked.input.clone().into_bytes();
        let host = self.alias();
        let spawned = match self {
            Transport::Ssh(ssh) => ssh.spawn(core::slice::from_ref(&asked.command)),
            Transport::Local { socket } => Err(SshError::Unreachable {
                host: host.clone(),
                detail: format!(
                    "{} names a socket, and a probe needs a host to run a command on",
                    socket.display()
                ),
            }),
        };
        async move {
            let mut spawned = spawned?;
            // Written beside the wait rather than before it: a script that
            // prints more than a pipe holds before it has read all of itself
            // would otherwise wait on this, and this on it.
            let writing = spawned.child.stdin.take();
            let feeding = async move {
                use tokio::io::AsyncWriteExt as _;
                let Some(mut writing) = writing else {
                    return;
                };
                if writing.write_all(&input).await.is_ok() {
                    let _closed = writing.shutdown().await;
                }
            };
            let ended = tokio::time::timeout(deadline, spawned.child.wait_with_output());
            let ((), waited) = tokio::join!(feeding, ended);
            let Ok(output) = waited else {
                return Err(ProbeError::from(SshError::Timeout {
                    host,
                    stage: doing.to_owned(),
                }));
            };
            let output = output.map_err(|source| ProbeError::Transport {
                detail: format!("{doing}: the command could not be waited for: {source}"),
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
/// [`ProbeError::Ssh`] or [`ProbeError::Transport`] when the script cannot be
/// run,
/// [`ProbeError::Unsupported`] for a machine iznik has no artifact for,
/// [`ProbeError::Unwritable`] when nowhere is writable, and
/// [`ProbeError::Malformed`] when the answer is not one this can read.
pub async fn probe(
    transport: &impl RunsRemotely,
    deadline: Duration,
) -> Result<HostProbe, ProbeError> {
    let posix = transport.run(&probe_command(), deadline).await;
    let windows_may_answer = match &posix {
        Ok(output) => windows_underneath(output),
        // Only a command that ran and failed says anything about the shell.
        // A host that refused the key, whose key changed, or that did not
        // answer at all would refuse a second command for the same reason,
        // and a second connection with a refused key is a second strike
        // towards whatever lockout the host keeps.
        Err(failure) => failure.ran_and_failed(),
    };
    if !windows_may_answer {
        return posix.and_then(|output| parse(&output));
    }
    // A Windows host's shell is `cmd.exe`, which cannot run the POSIX
    // script — or a POSIX layer on Windows, which runs it and says it is
    // not a machine iznik serves. The failure is kept when the PowerShell
    // probe fails too, so a Unix host that could not be asked is not
    // reported as a Windows one.
    let asked = RemoteScript::alone(crate::bootstrap::windows::probe_command());
    match transport.run(&asked, deadline).await {
        Ok(output) => parse(&output),
        Err(_windows) => posix.and_then(|output| parse(&output)),
    }
}

/// Whether a POSIX answer came from a layer on top of Windows.
fn windows_underneath(output: &str) -> bool {
    named(output, "system").is_some_and(|system| {
        WINDOWS_POSIX_LAYERS
            .iter()
            .any(|layer| system.starts_with(layer))
    })
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
        "Windows_NT" => Some(OperatingSystem::Windows),
        _other => None,
    }
}

/// The machine a `uname -m` names, if it is one iznik serves.
fn architecture(said: &str) -> Option<Architecture> {
    match said {
        "x86_64" | "amd64" | "AMD64" => Some(Architecture::X86_64),
        "aarch64" | "arm64" | "ARM64" => Some(Architecture::Aarch64),
        _other => None,
    }
}

/// The version line an installed server printed, read into what it says.
///
/// The line is `iznik-server <crate version> protocol <number>`, which is what
/// `--version` prints, and it must begin with that name: something else at
/// that path saying `myserver 9.9.9 protocol 1` is not an iznik server, and
/// reading it as one would put an upgrade decision on a stranger's words.
fn installed(said: &str) -> Option<InstalledServer> {
    let mut words = said.split_whitespace();
    let _named = words.next().filter(|word| *word == SERVER_NAME)?;
    let crate_version = words.next()?.to_owned();
    let _protocol = words.next().filter(|word| *word == PROTOCOL_WORD)?;
    let protocol_version = words.next()?.parse().ok()?;
    Some(InstalledServer {
        crate_version,
        protocol_version,
    })
}

/// How many hexadecimal digits a SHA-256 is written in.
const DIGEST_DIGITS: usize = 64;

/// A digest line's value, when it is a SHA-256 and not a stranger's words.
fn digest(said: &str) -> Option<String> {
    let digits = said.trim().to_ascii_lowercase();
    (digits.len() == DIGEST_DIGITS && digits.bytes().all(|digit| digit.is_ascii_hexdigit()))
        .then_some(digits)
}

/// One prefix the host was asked about: where, whether it may be written, and
/// what an iznik server there says it is.
struct Candidate {
    /// Where it is.
    path: PathBuf,
    /// Whether this user may write it.
    writable: bool,
    /// Whether the terminfo iznik carries is compiled *there*.
    terminfo: bool,
    /// The server installed *there*, if there is one this can read.
    server: Option<InstalledServer>,
    /// The SHA-256 of the server *there*, if the host could say.
    digest: Option<String>,
}

/// A candidate as its lines arrive, before it is known to be complete.
#[derive(Default)]
struct Building {
    /// Its `writable` line.
    writable: Option<bool>,
    /// Its `version` line.
    version: Option<String>,
    /// Its `terminfo` line.
    terminfo: Option<bool>,
    /// Its `path` line.
    path: Option<PathBuf>,
    /// Its `digest` line, which a host that predates it does not print.
    digest: Option<String>,
}

/// One `candidate <n> <name> <value>` line, put where it belongs.
fn read_field(held: &mut BTreeMap<usize, Building>, line: &str) {
    let Some(rest) = field(line.trim_end_matches('\r'), "candidate") else {
        return;
    };
    let Some((index, named)) = rest.split_once(FIELD_SEPARATOR) else {
        return;
    };
    let Ok(index) = index.parse::<usize>() else {
        return;
    };
    let Some((name, value)) = named.split_once(FIELD_SEPARATOR) else {
        return;
    };
    let building = held.entry(index).or_default();
    match name {
        "writable" => building.writable = Some(value == YES),
        "version" => building.version = Some(value.to_owned()),
        "terminfo" => building.terminfo = Some(value == YES),
        "path" => building.path = Some(PathBuf::from(value)),
        "digest" => building.digest = digest(value),
        _other => {}
    }
}

/// Every prefix the host was asked about, in the order it was asked.
///
/// A candidate is three labelled lines rather than one line of three fields,
/// because both the value that varies most — a prefix — and the one beside it
/// — a version line — may contain a space. With the value the whole rest of
/// its own line, neither can eat the other: a host whose `XDG_DATA_HOME` is
/// `/mnt/My Data` is read as it is, rather than as an unwritable `/mnt/My`.
fn candidates(output: &str) -> Vec<Candidate> {
    let mut held: BTreeMap<usize, Building> = BTreeMap::new();
    for line in output.lines() {
        read_field(&mut held, line);
    }
    held.into_values()
        .filter_map(|building| {
            let said = building.version.unwrap_or_else(|| NOTHING.to_owned());
            Some(Candidate {
                path: building.path?,
                writable: building.writable?,
                terminfo: building.terminfo?,
                server: Some(said.as_str())
                    .filter(|named| *named != NOTHING)
                    .and_then(installed),
                digest: building.digest,
            })
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
    // Every field the script prints must be there. A host that answered only
    // half of it is a host something went wrong on, and reading a missing
    // `tic` line as "no tic" would install nothing and say nothing about why.
    if named(output, "tic").is_none() {
        return Err(missing("tic"));
    }
    let offered = candidates(output);
    if offered.is_empty() {
        return Err(missing("candidate"));
    }
    let Some(chosen) = offered.iter().find(|candidate| candidate.writable) else {
        return Err(ProbeError::Unwritable {
            candidates: offered
                .into_iter()
                .map(|candidate| candidate.path)
                .collect(),
        });
    };
    Ok(HostProbe {
        operating_system,
        architecture,
        // The server at the prefix that was chosen, and not one at some other
        // candidate: what the bootstrap will run is `<prefix>/bin/iznik-server`,
        // so a server anywhere else is not the one it is deciding about.
        server: chosen.server.clone(),
        // And its bytes, for the same reason.
        server_digest: chosen.digest.clone(),
        // The terminfo at the prefix that was chosen, for the same reason the
        // server is: what a pane will be told about is `<prefix>/terminfo`,
        // and an entry under a candidate this user cannot write is not it.
        terminfo_installed: chosen.terminfo,
        tic_available: said_yes(output, "tic"),
        prefix: chosen.path.clone(),
        installed_at: offered
            .iter()
            .find(|candidate| candidate.server.is_some() || candidate.digest.is_some())
            .map(|candidate| candidate.path.clone()),
    })
}
