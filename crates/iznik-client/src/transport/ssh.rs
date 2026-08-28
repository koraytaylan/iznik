//! Spawning the system `ssh` with `ControlMaster`, and classifying its failures into messages a person can act on.
//!
//! What is *not* here is the point of it. iznik passes no `User`, no `Port`,
//! no `IdentityFile`, no `ProxyJump`, no `HostName` and none of the short
//! flags that mean the same things, because every one of those is something a
//! person may already have written in `~/.ssh/config` and reimplementing that
//! surface means getting it wrong for the first user whose setup is
//! interesting. What iznik adds is the four options it owns — a persistent
//! master so the second command is fast, a control path under its own runtime
//! directory, keepalives so a dead link is noticed in seconds, and a connect
//! timeout — and a test asserts the argument vector against that list.
//!
//! Three of the six options iznik does pass are ones a person could also have
//! written, and a command-line `-o` beats `~/.ssh/config`: a user with
//! `ServerAliveInterval 60` on a metered link gets five seconds times three
//! instead, and a `ConnectTimeout` of their own is replaced by fifteen. The
//! architecture owns those three deliberately — a link iznik cannot notice
//! dying is a session that hangs — and this note is here so the trade is a
//! decision on the record rather than a surprise.
//!
//! The other half is the classification. "Connection failed" tells a person
//! nothing, and a changed host key demands an alarming, specific message,
//! because the difference between a moved server and somebody in the middle is
//! the whole of the security this transport has.

use core::fmt::{self, Display, Formatter};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::transport::ClientRuntimePaths;

/// How long a master stays after the last command that used it, so the second
/// command to a host does not pay for a second handshake.
pub const CONTROL_PERSIST: Duration = Duration::from_mins(10);

/// How often `ssh` asks a live connection whether it is still there.
pub const SERVER_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// How many of those may go unanswered before `ssh` gives up. Three at five
/// seconds is fifteen, which is the transport's own patience; the channel's
/// application-level ping is what notices sooner.
pub const SERVER_ALIVE_COUNT_MAXIMUM: u32 = 3;

/// How long a connection may take to establish.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The least any of these may be given to `ssh` as. Its options are whole
/// seconds, and a zero is not "at once" to it but "never": `ControlPersist=0`
/// keeps a master indefinitely, `ConnectTimeout=0` falls back to the system's
/// own, `ServerAliveInterval=0` sends no keepalives at all. So a field set to
/// something under a second is rounded up rather than truncated into its
/// opposite.
const LEAST_SECONDS: u64 = 1;

/// The program, which is the system's and never a library.
const PROGRAM: &str = "ssh";

/// The flag every option is given with.
const OPTION_FLAG: &str = "-o";

/// The flag that ends a master.
const CONTROL_FLAG: &str = "-O";

/// What `-O` is given to end one.
const EXIT_COMMAND: &str = "exit";

/// The variable that names the program `ssh` asks for a passphrase with.
const ASKPASS_VARIABLE: &str = "SSH_ASKPASS";

/// The variable that makes `ssh` use it even with a terminal attached, so a
/// prompt reaches the application's helper rather than a console nobody is
/// looking at.
const ASKPASS_REQUIRE_VARIABLE: &str = "SSH_ASKPASS_REQUIRE";

/// What that variable is set to.
const ASKPASS_REQUIRE: &str = "force";

/// Every timing this transport runs under, so a test can shorten any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshOptions {
    /// The program `ssh` asks for a passphrase with, when the application
    /// provides one.
    pub askpass_program: Option<PathBuf>,
    /// How long a master outlives the last command that used it.
    pub control_persist: Duration,
    /// How often a live connection is asked whether it is there.
    pub server_alive_interval: Duration,
    /// How many such questions may go unanswered.
    pub server_alive_count_maximum: u32,
    /// How long establishing a connection may take.
    pub connect_timeout: Duration,
}

impl Default for SshOptions {
    fn default() -> SshOptions {
        SshOptions {
            askpass_program: None,
            control_persist: CONTROL_PERSIST,
            server_alive_interval: SERVER_ALIVE_INTERVAL,
            server_alive_count_maximum: SERVER_ALIVE_COUNT_MAXIMUM,
            connect_timeout: CONNECT_TIMEOUT,
        }
    }
}

/// A host reached through the system `ssh`.
#[derive(Clone, Debug)]
pub struct SshTransport {
    /// The alias, exactly as the user wrote it.
    alias: String,
    /// Where this alias's control socket lives.
    control_path: PathBuf,
    /// The timings.
    options: SshOptions,
}

/// A running `ssh`, and the alias it was for.
#[derive(Debug)]
pub struct SshChild {
    /// The process.
    pub child: Child,
    /// The host it is talking to, for whatever has to name it.
    pub alias: String,
}

/// Why a command over `ssh` did not work, in the terms a person can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshError {
    /// Nothing answered: no route, no listener, or a name that resolves to
    /// nothing.
    Unreachable {
        /// The alias.
        host: String,
        /// What `ssh` said.
        detail: String,
    },
    /// The host answered and refused the credentials offered.
    AuthenticationFailed {
        /// The alias.
        host: String,
        /// What `ssh` said.
        detail: String,
    },
    /// The host's key is not the one `known_hosts` remembers.
    HostKeyChanged {
        /// The alias.
        host: String,
        /// What `ssh` said.
        detail: String,
    },
    /// The connection worked and the command did not.
    RemoteCommandFailed {
        /// The alias.
        host: String,
        /// What it exited with.
        status: i32,
        /// What it said.
        stderr: String,
    },
    /// `ssh` itself could not be started.
    Spawn {
        /// What the operating system said.
        detail: String,
    },
    /// A stage took longer than it was given.
    Timeout {
        /// The alias.
        host: String,
        /// What was being done.
        stage: String,
    },
}

impl Display for SshError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SshError::Unreachable { host, detail } => {
                write!(formatter, "{host} could not be reached: {detail}")
            }
            SshError::AuthenticationFailed { host, detail } => write!(
                formatter,
                "{host} refused the credentials offered: {detail}"
            ),
            SshError::HostKeyChanged { host, detail } => write!(
                formatter,
                "the host key for {host} was not accepted: {detail} \
                 If it was remembered before and has changed, either the host \
                 was rebuilt or something is answering in its place; do not \
                 connect until you know which."
            ),
            SshError::RemoteCommandFailed {
                host,
                status,
                stderr,
            } => write!(formatter, "on {host} the command exited {status}: {stderr}"),
            SshError::Spawn { detail } => write!(formatter, "ssh could not be started: {detail}"),
            SshError::Timeout { host, stage } => {
                write!(formatter, "{host} did not answer while {stage}")
            }
        }
    }
}

impl core::error::Error for SshError {}

/// What `ssh` says when the key it was offered is not one the host accepts.
const REFUSED_MARKERS: &[&str] = &[
    "permission denied",
    "too many authentication failures",
    "no supported authentication methods",
];

/// What it says when the key the host offered is not one this machine will
/// accept — because it is not the one remembered, or because none is
/// remembered and strict checking was asked for. Both are the host's key and
/// neither is the network, so both are told apart from being unreachable; the
/// detail carried is `ssh`'s own last line, which says which it was.
const CHANGED_MARKERS: &[&str] = &[
    "remote host identification has changed",
    "host key verification failed",
];

/// What it says when nothing answered.
const UNREACHABLE_MARKERS: &[&str] = &[
    "connection refused",
    "connection timed out",
    "no route to host",
    "could not resolve hostname",
    "name or service not known",
    "network is unreachable",
    "operation timed out",
];

/// The status `ssh` exits with when it is `ssh` itself that failed, rather
/// than the command it ran.
const SSH_OWN_FAILURE: i32 = 255;

impl SshTransport {
    /// A transport for `alias`, with its control socket under `paths`.
    #[must_use]
    pub fn new(alias: &str, paths: &ClientRuntimePaths, options: SshOptions) -> SshTransport {
        SshTransport {
            control_path: paths.control_path(alias),
            alias: alias.to_owned(),
            options,
        }
    }

    /// The alias, as the user wrote it.
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Where this alias's control socket lives.
    #[must_use]
    pub fn control_path(&self) -> &PathBuf {
        &self.control_path
    }

    /// The whole argument vector for running `remote_command` on this host:
    /// the options iznik owns, then the alias, then the command.
    ///
    /// Public because what is *not* in it is a property worth asserting, and
    /// asserting it needs no process.
    #[must_use]
    pub fn arguments(&self, remote_command: &[String]) -> Vec<String> {
        let mut arguments = Vec::new();
        for option in [
            "ControlMaster=auto".to_owned(),
            format!("ControlPath={}", self.control_path.display()),
            format!("ControlPersist={}", seconds(self.options.control_persist)),
            format!(
                "ServerAliveInterval={}",
                seconds(self.options.server_alive_interval)
            ),
            format!(
                "ServerAliveCountMax={}",
                self.options.server_alive_count_maximum
            ),
            format!("ConnectTimeout={}", seconds(self.options.connect_timeout)),
        ] {
            arguments.push(OPTION_FLAG.to_owned());
            arguments.push(option);
        }
        arguments.push(self.alias.clone());
        arguments.extend(remote_command.iter().cloned());
        arguments
    }

    /// Runs `remote_command` on this host, with its standard streams piped.
    ///
    /// # Errors
    ///
    /// [`SshError::Spawn`] when `ssh` itself cannot be started.
    pub fn spawn(&self, remote_command: &[String]) -> Result<SshChild, SshError> {
        let mut command = Command::new(PROGRAM);
        command
            .args(self.arguments(remote_command))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(askpass) = &self.options.askpass_program {
            command
                .env(ASKPASS_VARIABLE, askpass)
                .env(ASKPASS_REQUIRE_VARIABLE, ASKPASS_REQUIRE);
        }
        let child = command.spawn().map_err(|source| SshError::Spawn {
            detail: source.to_string(),
        })?;
        Ok(SshChild {
            child,
            alias: self.alias.clone(),
        })
    }

    /// Ends the master for this host, so nothing outlives a client that has
    /// finished with it.
    ///
    /// # Errors
    ///
    /// [`SshError::Spawn`] when `ssh` itself cannot be started.
    pub fn close_master(&self) -> Result<SshChild, SshError> {
        let mut arguments = self.arguments(&[]);
        // Before the alias, which is the last thing `arguments` puts there.
        let at = arguments.len().saturating_sub(1);
        arguments.splice(at..at, [CONTROL_FLAG.to_owned(), EXIT_COMMAND.to_owned()]);
        let mut command = Command::new(PROGRAM);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().map_err(|source| SshError::Spawn {
            detail: source.to_string(),
        })?;
        Ok(SshChild {
            child,
            alias: self.alias.clone(),
        })
    }
}

/// A duration as the whole seconds `ssh` takes, never fewer than
/// [`LEAST_SECONDS`].
fn seconds(held: Duration) -> u64 {
    held.as_secs().max(LEAST_SECONDS)
}

/// What an `ssh` that exited non-zero was actually telling you.
///
/// A pure function over the status and what it said, so the captured output of
/// a refused connection, an unresolvable name, a denied key and a changed host
/// key are a table of cases rather than five hosts to arrange.
#[must_use]
pub fn classify(host: &str, status: Option<i32>, stderr: &str) -> SshError {
    // The status decides first, and only then the words. `ssh` reports its own
    // failures as 255 and passes anything else through from the command it ran
    // — so a remote binary that is not executable exits 126 saying "Permission
    // denied", and reading that as a refused key would tell a person to look
    // at their credentials for a file mode. What the command said is the
    // command's, whatever words are in it.
    let (Some(SSH_OWN_FAILURE) | None) = status else {
        return SshError::RemoteCommandFailed {
            host: host.to_owned(),
            status: status.unwrap_or(SSH_OWN_FAILURE),
            stderr: stderr.trim_end().to_owned(),
        };
    };
    for (markers, name) in [
        (CHANGED_MARKERS, Refusal::HostKey),
        (REFUSED_MARKERS, Refusal::Credentials),
        (UNREACHABLE_MARKERS, Refusal::Link),
    ] {
        if let Some(detail) = matching_line(stderr, markers) {
            return name.into_error(host, detail);
        }
    }
    // `ssh`'s own status with words nobody here knows: the link, because that
    // is what `ssh` failing rather than the command means.
    SshError::Unreachable {
        host: host.to_owned(),
        detail: last_line(stderr),
    }
}

/// Which of the three the words named.
#[derive(Clone, Copy)]
enum Refusal {
    /// The host's key.
    HostKey,
    /// The credentials offered.
    Credentials,
    /// The link itself.
    Link,
}

impl Refusal {
    /// The error it stands for, about `host`, carrying `detail`.
    fn into_error(self, host: &str, detail: String) -> SshError {
        let host = host.to_owned();
        match self {
            Refusal::HostKey => SshError::HostKeyChanged { host, detail },
            Refusal::Credentials => SshError::AuthenticationFailed { host, detail },
            Refusal::Link => SshError::Unreachable { host, detail },
        }
    }
}

/// The first line that carries one of `markers`, which is the line that says
/// *why*.
///
/// Not the last line: `ssh` ends both host-key failures with the same generic
/// `Host key verification failed.`, so carrying that would give a first
/// connection to an unknown host and a key that has changed under someone the
/// same words — and those two want opposite reactions from a person.
fn matching_line(stderr: &str, markers: &[&str]) -> Option<String> {
    stderr.lines().map(str::trim).find_map(|line| {
        let said = line.to_lowercase();
        markers
            .iter()
            .any(|marker| said.contains(marker))
            .then(|| line.to_owned())
    })
}

/// The last thing `ssh` said that was not empty, for when nothing it said was
/// recognized at all.
fn last_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}
