//! Spawning the system `ssh` with `ControlMaster`, and classifying its failures into messages a person can act on.
//!
//! What is *not* here is the point of it. iznik passes no `User`, no `Port`,
//! no `IdentityFile`, no `ProxyJump`, no `HostName` and none of the short
//! flags that mean the same things, because every one of those is something a
//! person may already have written in `~/.ssh/config` and reimplementing that
//! surface means getting it wrong for the first user whose setup is
//! interesting. What iznik adds is the six settings it owns — a persistent
//! master so the second command is fast (`ControlMaster`, `ControlPersist`,
//! and a `ControlPath` under its own runtime directory), keepalives so a dead
//! link is noticed in seconds (`ServerAliveInterval`, `ServerAliveCountMax`),
//! and a `ConnectTimeout` — plus the three overrides below and, when the
//! application gave no askpass program, `BatchMode=yes`; a test asserts the
//! argument vector against that list.
//!
//! Three of those six — the keepalives and the timeout — are ones a person is
//! likely to have written too, and a command-line `-o` beats `~/.ssh/config`:
//! a user with `ServerAliveInterval 60` on a metered link gets five seconds
//! times three instead, and a `ConnectTimeout` of their own is replaced by
//! fifteen. The architecture owns those three deliberately — a link iznik
//! cannot notice dying is a session that hangs — and this note is here so the
//! trade is a decision on the record rather than a surprise.
//!
//! Three more are overrides rather than settings, and they exist because a
//! person's configuration can break iznik's own commands without being wrong
//! for anything else: `RequestTTY=no`, because a terminal allocated for a
//! `RequestTTY yes` host would mangle the binary protocol and echo the upload
//! back; `RemoteCommand=none`, because a `RemoteCommand` in a `Host` block
//! would run instead of the command iznik asked for; and
//! `ClearAllForwardings=yes`, because a `LocalForward` that cannot bind fails
//! the connection, and one that can would be held open by every probe and
//! relay iznik starts. None of them changes how the host is reached.
//!
//! The last, `BatchMode=yes`, is there only when nothing can ask a person
//! anything: a passphrase, a password or a host key nobody has accepted then
//! fails at once and says which, instead of prompting a terminal nobody is
//! looking at until the connection's deadline.
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

/// How long ending a master may take before it is left to `ControlPersist`.
///
/// `ssh -O exit` talks to a socket on this machine and answers in
/// milliseconds; a master that does not answer in two seconds is one that
/// will be gone by itself when its persistence runs out.
pub const CLOSE_DEADLINE: Duration = Duration::from_secs(2);

/// The least any of these may be given to `ssh` as. Its options are whole
/// seconds, and a zero is not "at once" to it but "never": `ControlPersist=0`
/// keeps a master indefinitely, `ConnectTimeout=0` falls back to the system's
/// own, `ServerAliveInterval=0` sends no keepalives at all. So a field set to
/// something under a second is rounded up rather than truncated into its
/// opposite.
const LEAST_SECONDS: u64 = 1;

/// The program, which is the system's and never a library.
const PROGRAM: &str = "ssh";

/// Whether this machine's `ssh` can multiplex sessions on one connection.
///
/// Win32-OpenSSH still cannot. Asking it for `ControlMaster` fails the
/// connection, so a Windows build leaves those options off.
#[must_use]
pub fn master_is_available() -> bool {
    cfg!(unix)
}

/// What iznik's own commands need whatever a person configured: no terminal,
/// no command of the configuration's instead of iznik's, and no forwarding.
/// The module documentation says why each.
pub const OVERRIDES: &[&str] = &[
    "RequestTTY=no",
    "RemoteCommand=none",
    "ClearAllForwardings=yes",
];

/// What makes `ssh` fail rather than ask, when there is no askpass program to
/// ask through.
pub const BATCH_MODE: &str = "BatchMode=yes";

/// The flag every option is given with.
const OPTION_FLAG: &str = "-o";

/// What ends `ssh`'s options, so that what follows is the host whatever it
/// begins with.
pub const END_OF_OPTIONS: &str = "--";

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
    /// provides one. Without one, `ssh` runs with [`BATCH_MODE`]: whatever
    /// would have needed a person — a passphrase, a password, a host key
    /// nobody has accepted — fails at once and says which.
    pub askpass_program: Option<PathBuf>,
    /// How long a master outlives the last command that used it.
    pub control_persist: Duration,
    /// How often a live connection is asked whether it is there.
    pub server_alive_interval: Duration,
    /// How many such questions may go unanswered.
    pub server_alive_count_maximum: u32,
    /// How long establishing a connection may take.
    pub connect_timeout: Duration,
    /// How long ending a master may take.
    pub close_deadline: Duration,
}

impl Default for SshOptions {
    fn default() -> SshOptions {
        SshOptions {
            askpass_program: None,
            control_persist: CONTROL_PERSIST,
            server_alive_interval: SERVER_ALIVE_INTERVAL,
            server_alive_count_maximum: SERVER_ALIVE_COUNT_MAXIMUM,
            connect_timeout: CONNECT_TIMEOUT,
            close_deadline: CLOSE_DEADLINE,
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
    /// `known_hosts` remembers no key for the host, and nobody was there to
    /// accept the one it offered.
    ///
    /// Its own error, because it wants the opposite reaction from a changed
    /// key: this is every host's first connection, and the answer is to
    /// connect once by hand and check the fingerprint.
    HostKeyUnknown {
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
                "the host key for {host} has changed: {detail} \
                 Either the host was rebuilt or something is answering in its \
                 place; do not connect until you know which."
            ),
            SshError::HostKeyUnknown { host, detail } => write!(
                formatter,
                "the host key for {host} is not known yet: {detail} \
                 Run `ssh {host}` once in a terminal, check the fingerprint it \
                 shows, accept it, and then reconnect."
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
///
/// A refusal of credentials in the form authentication gives it — `Permission
/// denied (publickey,password).` — and not the bare words, which `ssh` also
/// says of a file it could not open or a control socket it could not bind.
/// Those are no reason to park a host until somebody changes a key.
const REFUSED_MARKERS: &[&str] = &[
    "permission denied (",
    "too many authentication failures",
    "no supported authentication methods",
];

/// What it says when the key the host offered is not the one remembered.
///
/// Looked for before [`UNKNOWN_MARKERS`], because a changed key's warning ends
/// with the same generic line an unknown one does.
const CHANGED_MARKERS: &[&str] = &[
    "remote host identification has changed",
    "has changed and you have requested strict checking",
];

/// What it says when no key is remembered for the host and nobody accepted the
/// one it offered: the batch-mode refusal, the question an askpass program was
/// asked and declined, and — when neither of those lines is there — the
/// generic last line both host-key failures end with, which is read as the
/// milder of the two because a changed key always says so first.
const UNKNOWN_MARKERS: &[&str] = &[
    "host key is known for",
    "the authenticity of host",
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
    /// the options iznik owns, then [`END_OF_OPTIONS`] and the alias, then the
    /// command.
    ///
    /// Public because what is *not* in it is a property worth asserting, and
    /// asserting it needs no process. A Unix `ssh` is asked to multiplex.
    /// Win32-OpenSSH cannot: its control socket would have to pass file
    /// descriptors, which it does not, so that build opens a connection per
    /// command and still sends the keepalives and the connect timeout.
    #[must_use]
    pub fn arguments(&self, remote_command: &[String]) -> Vec<String> {
        self.arguments_with_master(remote_command, master_is_available())
    }

    /// [`arguments`](Self::arguments) with multiplexing chosen by the caller,
    /// so the Windows command line can be asserted on a Unix machine.
    #[must_use]
    pub fn arguments_with_master(&self, remote_command: &[String], master: bool) -> Vec<String> {
        let mut arguments = self.options(master);
        arguments.extend(self.destination());
        arguments.extend(remote_command.iter().cloned());
        arguments
    }

    /// The whole argument vector that ends this host's master: the options
    /// iznik owns, the control command, then the alias.
    ///
    /// Public for the reason [`arguments`](Self::arguments) is: that the
    /// alias still comes after [`END_OF_OPTIONS`] here is worth asserting.
    #[must_use]
    pub fn close_arguments(&self) -> Vec<String> {
        let mut arguments = self.options(master_is_available());
        arguments.push(CONTROL_FLAG.to_owned());
        arguments.push(EXIT_COMMAND.to_owned());
        arguments.extend(self.destination());
        arguments
    }

    /// The options iznik owns, each after its flag.
    fn options(&self, master: bool) -> Vec<String> {
        let mut options = Vec::new();
        if master {
            options.push("ControlMaster=auto".to_owned());
            options.push(format!(
                "ControlPath={}",
                control_path_option(&self.control_path)
            ));
            options.push(format!(
                "ControlPersist={}",
                seconds(self.options.control_persist)
            ));
        }
        options.push(format!(
            "ServerAliveInterval={}",
            seconds(self.options.server_alive_interval)
        ));
        options.push(format!(
            "ServerAliveCountMax={}",
            self.options.server_alive_count_maximum
        ));
        options.push(format!(
            "ConnectTimeout={}",
            seconds(self.options.connect_timeout)
        ));
        options.extend(OVERRIDES.iter().map(|forced| (*forced).to_owned()));
        // With nothing to ask a person through, nothing may be asked: a
        // prompt would go to a terminal nobody is looking at, or to none, and
        // the connection would hang until its deadline instead of failing at
        // once with words that say why.
        if self.options.askpass_program.is_none() {
            options.push(BATCH_MODE.to_owned());
        }
        options
            .into_iter()
            .flat_map(|option| vec![OPTION_FLAG.to_owned(), option])
            .collect()
    }

    /// The end of the options and the alias after it.
    ///
    /// Whatever a person typed is a host and never an option: without the
    /// marker, an alias such as `-oProxyCommand=…` would be read by `ssh` as
    /// one of its own options, and that one runs a command on this machine.
    fn destination(&self) -> Vec<String> {
        vec![END_OF_OPTIONS.to_owned(), self.alias.clone()]
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

    /// Ends the master for this host, if this machine's `ssh` keeps one, and
    /// waits for `ssh` to finish — for at most the close deadline, past which
    /// the master is left to go when its persistence runs out.
    ///
    /// Nothing to report: a host with no master running is already what this
    /// asks for.
    pub async fn end_master(&self) {
        if !master_is_available() {
            return;
        }
        let Ok(mut ending) = self.close_master() else {
            return;
        };
        let _waited = tokio::time::timeout(self.options.close_deadline, ending.child.wait()).await;
    }

    /// Ends the master for this host, so nothing outlives a client that has
    /// finished with it.
    ///
    /// # Errors
    ///
    /// [`SshError::Spawn`] when `ssh` itself cannot be started.
    pub fn close_master(&self) -> Result<SshChild, SshError> {
        let arguments = self.close_arguments();
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

/// A control path as `ssh` reads it back unchanged from an `-o` option.
///
/// `ssh` reads an option's value the way it reads a line of its own
/// configuration: split at whitespace unless quoted, with `%` starting one of
/// its own tokens. The path is under a runtime directory, and that is whatever
/// `TMPDIR` or the application said — `/Users/Jane Doe/...` is a directory
/// somebody has. So it is double-quoted, a quote or backslash in it is escaped
/// the way `ssh`'s own argument splitting reads them, and a `%` is doubled.
#[must_use]
pub fn control_path_option(path: &std::path::Path) -> String {
    let mut quoted = String::from(QUOTE);
    for character in path.display().to_string().chars() {
        match character {
            QUOTE | ESCAPE => {
                quoted.push(ESCAPE);
                quoted.push(character);
            }
            TOKEN => {
                quoted.push(TOKEN);
                quoted.push(TOKEN);
            }
            other => quoted.push(other),
        }
    }
    quoted.push(QUOTE);
    quoted
}

/// What `ssh` quotes an option's value with.
const QUOTE: char = '"';

/// What it escapes a quote inside one with.
const ESCAPE: char = '\\';

/// What begins one of its own tokens in a path.
const TOKEN: char = '%';

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
        (CHANGED_MARKERS, Refusal::ChangedKey),
        (UNKNOWN_MARKERS, Refusal::UnknownKey),
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

/// Which of the four the words named.
#[derive(Clone, Copy)]
enum Refusal {
    /// The host's key, which is not the one remembered.
    ChangedKey,
    /// The host's key, when none is remembered.
    UnknownKey,
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
            Refusal::ChangedKey => SshError::HostKeyChanged { host, detail },
            Refusal::UnknownKey => SshError::HostKeyUnknown { host, detail },
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
