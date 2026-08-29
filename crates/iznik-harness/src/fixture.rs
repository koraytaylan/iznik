//! The two-container fixture: per-run credentials, readiness under a cap,
//! commands, faults, a measured start time, and orphans made impossible to
//! keep. A host is a root `sshd` with the unprivileged user `iznik`; the
//! engine is a bare machine that speaks real SSH to each host by name over a
//! private network, with an ed25519 key pair generated inside it and host
//! keys generated inside each host, so no credential exists outside the
//! containers. Every container and network carries the run's prefix and the
//! owner's process id as labels and a podman timeout of its own; a start
//! first removes whatever a dead owner left behind.

use std::fmt::{self, Display, Formatter, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::deadline::{DeadlineError, wait_until};
use crate::images::{IMAGE_BUILD_DEADLINE, ImagesError, PROGRAM, ensure_images};
use crate::process::{self, Completed, Deadline, Output, ProcessError};

/// How long both hosts may take to answer over SSH.
pub const SSH_READY_CAP: Duration = Duration::from_secs(30);

/// How often readiness is asked.
pub const SSH_READY_INTERVAL: Duration = Duration::from_millis(100);

/// How long podman lets a container live, whatever became of its run.
pub const CONTAINER_TIMEOUT: Duration = Duration::from_mins(10);

/// The most a start may take with warm images: the harness's own claim.
pub const FIXTURE_START_CEILING: Duration = Duration::from_secs(10);

/// The label naming the run a container or network belongs to.
pub const RUN_LABEL: &str = "iznik.run";

/// The label naming the process that owns a container or network.
pub const OWNER_LABEL: &str = "iznik.owner";

/// The engine's alias.
pub const ENGINE_ALIAS: &str = "engine";

/// Where the staged directory is mounted, read-only, in every container.
pub const MOUNT_POINT: &str = "/iznik";

/// The unprivileged user commands run as.
const USER: &str = "iznik";

/// The prefix of a host's alias, followed by its index.
const HOST_ALIAS_PREFIX: &str = "host";

/// The seconds an SSH connection attempt waits, so a command against a
/// disconnected host fails in seconds.
const CONNECT_TIMEOUT_SECONDS: &str = "3";

/// How long one engine command of the fixture's own may take.
const ENGINE_DEADLINE: Duration = Duration::from_mins(1);

/// The engine's private key.
const IDENTITY: &str = "/home/iznik/.ssh/id_ed25519";

/// The engine's main process: nothing, kept alive.
///
/// Public so that a case can say what a container must *not* start with: it
/// never waits on anything, so a container whose first process is this one
/// keeps every orphan it is given for ever.
pub const IDLE_PROGRAM: &str = "sleep";

/// The idle program's argument: for ever.
const IDLE_ARGUMENT: &str = "infinity";

/// Runs of this process, so each fixture's prefix is its own.
static RUNS: AtomicUsize = AtomicUsize::new(0);

/// How a fixture is started; every timing is a field with a named default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureOptions {
    /// How many hosts, aliased `host0`, `host1`, and so on.
    pub hosts: usize,
    /// The staged directory mounted read-only at [`MOUNT_POINT`].
    pub staged: PathBuf,
    /// How long the hosts may take to answer over SSH.
    pub ssh_ready_cap: Duration,
    /// How often readiness is asked.
    pub ssh_ready_interval: Duration,
    /// How long podman lets each container live.
    pub container_timeout: Duration,
    /// Whether the hosts run their `sshd`; `false` only proves the readiness
    /// cap, by a host that never answers.
    pub host_daemon: bool,
}

impl FixtureOptions {
    /// The defaults for some hosts and a staged directory.
    #[must_use]
    pub fn new(hosts: usize, staged: PathBuf) -> FixtureOptions {
        FixtureOptions {
            hosts,
            staged,
            ssh_ready_cap: SSH_READY_CAP,
            ssh_ready_interval: SSH_READY_INTERVAL,
            container_timeout: CONTAINER_TIMEOUT,
            host_daemon: true,
        }
    }
}

/// A process inside a container, by id or by the file a step wrote it to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Process {
    /// A known process id.
    Id(u32),
    /// A file inside the container holding the id, as a step's `$$`.
    IdFile(PathBuf),
}

/// A fault injected into a running fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Detach a container from the network.
    DisconnectNetwork {
        /// The container's alias.
        container: String,
    },
    /// Reattach a container to the network; its address may change.
    ReconnectNetwork {
        /// The container's alias.
        container: String,
    },
    /// Stop a process.
    PauseProcess {
        /// The container's alias.
        container: String,
        /// The process.
        process: Process,
    },
    /// Continue a stopped process.
    ResumeProcess {
        /// The container's alias.
        container: String,
        /// The process.
        process: Process,
    },
    /// Kill a process.
    KillProcess {
        /// The container's alias.
        container: String,
        /// The process.
        process: Process,
    },
}

/// What a reap removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reap {
    /// What belongs to a process that no longer exists.
    DeadOwners,
    /// Everything with the fixture's labels, for a person.
    Everything,
}

/// Why a fixture could not do what was asked.
#[derive(Debug)]
pub enum FixtureError {
    /// The images could not be ensured.
    Images(ImagesError),
    /// The container engine could not do what was asked.
    Engine {
        /// What was asked.
        action: String,
        /// What went wrong, boxed to keep the error small.
        source: Box<ProcessError>,
    },
    /// A host did not answer over SSH within the cap.
    SshNotReady {
        /// The host's alias.
        alias: String,
        /// The cap.
        cap: Duration,
        /// What the last attempt said.
        last_attempt: String,
        /// The wait's own account.
        source: DeadlineError,
    },
    /// No container has the alias.
    UnknownContainer {
        /// The alias asked for.
        alias: String,
    },
    /// A process id file did not hold one.
    ProcessId {
        /// The file.
        path: PathBuf,
        /// What it held.
        content: String,
    },
}

impl Display for FixtureError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            FixtureError::Images(source) => write!(formatter, "the images: {source}"),
            FixtureError::Engine { action, source } => {
                write!(formatter, "`{PROGRAM} {action}` failed: {source}")
            }
            FixtureError::SshNotReady {
                alias,
                cap,
                last_attempt,
                source,
            } => write!(
                formatter,
                "{alias} did not answer over SSH within {cap:?}: {source}; the last attempt said `{last_attempt}`"
            ),
            FixtureError::UnknownContainer { alias } => {
                write!(formatter, "no container is aliased {alias}")
            }
            FixtureError::ProcessId { path, content } => write!(
                formatter,
                "{} holds `{content}`, not a process id",
                path.display()
            ),
        }
    }
}

impl std::error::Error for FixtureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FixtureError::Images(source) => Some(source),
            FixtureError::Engine { source, .. } => Some(source.as_ref()),
            FixtureError::SshNotReady { source, .. } => Some(source),
            FixtureError::UnknownContainer { .. } | FixtureError::ProcessId { .. } => None,
        }
    }
}

impl From<ImagesError> for FixtureError {
    fn from(source: ImagesError) -> FixtureError {
        FixtureError::Images(source)
    }
}

/// One engine command, captured, under a deadline.
///
/// # Errors
///
/// [`FixtureError::Engine`] when it fails, naming what was asked.
fn podman(arguments: &[&str], deadline: Duration) -> Result<Completed, FixtureError> {
    let mut command = Command::new(PROGRAM);
    command.args(arguments);
    process::run(command, Deadline(deadline), Output::Capture).map_err(|source| {
        FixtureError::Engine {
            action: arguments.join(" "),
            source: Box::new(source),
        }
    })
}

/// The standard output of a completed command as text.
fn text(completed: &Completed) -> String {
    String::from_utf8_lossy(&completed.stdout).into_owned()
}

/// Whether a process id still names a process.
fn owner_alive(owner: &str) -> bool {
    owner
        .trim()
        .parse::<u32>()
        .is_ok_and(|id| Path::new("/proc").join(id.to_string()).exists())
}

/// The owner label's value in a rendered label set. Podman renders a
/// container's labels and a network's differently — a map here, a joined
/// string there — so the value is found by scanning for the label named
/// [`OWNER_LABEL`], after either an `=` or a `:`, between any of the
/// separators a rendering uses.
fn owner_of(labels: &str) -> &str {
    let after_equals = format!("{OWNER_LABEL}=");
    let after_colon = format!("{OWNER_LABEL}:");
    labels
        .split([',', ' ', '[', ']'])
        .find_map(|token| {
            token
                .strip_prefix(&after_equals)
                .or_else(|| token.strip_prefix(&after_colon))
        })
        .unwrap_or_default()
}

/// Removes a container or network now — `--time 0`, since a container's
/// process is its own init and ignores a polite signal — treating one
/// already gone (a concurrent teardown took it) as removed.
///
/// # Errors
///
/// [`FixtureError::Engine`] for any failure but the thing being absent.
fn remove(arguments: &[&str]) -> Result<(), FixtureError> {
    match podman(arguments, ENGINE_DEADLINE) {
        Ok(_removed) => Ok(()),
        Err(FixtureError::Engine { source, .. })
            if matches!(source.as_ref(), ProcessError::Failed { stderr_tail, .. }
                if stderr_tail.contains("no such") || stderr_tail.contains("unable to find")) =>
        {
            Ok(())
        }
        Err(other) => Err(other),
    }
}

/// Removes containers and networks with the fixture's labels — those of a
/// dead owner, or all of them — and says how many; a thing that vanishes
/// between the listing and its removal counts as removed.
///
/// # Errors
///
/// [`FixtureError::Engine`] when the engine cannot list or remove.
pub fn reap(scope: Reap) -> Result<usize, FixtureError> {
    let owner_filter = format!("label={OWNER_LABEL}");
    let mut removed: usize = 0;
    let containers = podman(
        &[
            "ps",
            "--all",
            "--filter",
            &owner_filter,
            "--format",
            "{{.Names}}\t{{.Labels}}",
        ],
        ENGINE_DEADLINE,
    )?;
    for line in text(&containers).lines() {
        let (name, labels) = line.split_once('\t').unwrap_or((line, ""));
        if scope == Reap::Everything || !owner_alive(owner_of(labels)) {
            remove(&["rm", "--force", "--time", "0", "--ignore", name])?;
            removed = removed.saturating_add(1);
        }
    }
    let networks = podman(
        &[
            "network",
            "ls",
            "--filter",
            &owner_filter,
            "--format",
            "{{.Name}}\t{{.Labels}}",
        ],
        ENGINE_DEADLINE,
    )?;
    for line in text(&networks).lines() {
        let (name, labels) = line.split_once('\t').unwrap_or((line, ""));
        if scope == Reap::Everything || !owner_alive(owner_of(labels)) {
            remove(&["network", "rm", "--force", "--time", "0", name])?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

/// The two containers, running, until dropped.
#[derive(Debug)]
pub struct Fixture {
    /// The run's prefix: every name and label carries it.
    prefix: String,
    /// The per-run network.
    network: String,
    /// The containers by alias and name, hosts first, the engine last.
    containers: Vec<(String, String)>,
    /// How the fixture was started.
    options: FixtureOptions,
    /// From `start` to every host answering over SSH.
    elapsed_start: Duration,
}

impl Fixture {
    /// The alias of the host at an index: `host0`, `host1`, and so on.
    #[must_use]
    pub fn host_alias(index: usize) -> String {
        format!("{HOST_ALIAS_PREFIX}{index}")
    }

    /// Starts the network and the containers, generates the credentials
    /// inside them, and waits for every host to answer over SSH; on any
    /// failure what was started is torn down.
    ///
    /// # Errors
    ///
    /// [`FixtureError`] as the step that failed names it; in particular
    /// [`FixtureError::SshNotReady`] when a host does not answer within
    /// `ssh_ready_cap`.
    pub fn start(options: FixtureOptions) -> Result<Fixture, FixtureError> {
        let started = Instant::now();
        let images = ensure_images(Deadline(IMAGE_BUILD_DEADLINE))?;
        reap(Reap::DeadOwners)?;
        let run = RUNS.fetch_add(1, Ordering::SeqCst);
        let prefix = format!("iznik-{}-{run}", std::process::id());
        let network = format!("{prefix}-net");
        let mut fixture = Fixture {
            prefix: prefix.clone(),
            network: network.clone(),
            containers: Vec::new(),
            options,
            elapsed_start: Duration::ZERO,
        };
        let owner = format!("{OWNER_LABEL}={}", std::process::id());
        let run_label = format!("{RUN_LABEL}={prefix}");
        podman(
            &[
                "network", "create", "--label", &run_label, "--label", &owner, &network,
            ],
            ENGINE_DEADLINE,
        )?;
        for index in 0..fixture.options.hosts {
            fixture.run_container(&Fixture::host_alias(index), &images.host, &[])?;
        }
        fixture.run_container(ENGINE_ALIAS, &images.engine, &[IDLE_PROGRAM, IDLE_ARGUMENT])?;
        fixture.provision()?;
        fixture.await_ssh()?;
        fixture.elapsed_start = started.elapsed();
        Ok(fixture)
    }

    /// Starts one container on the network with the run's labels, its
    /// timeout and the staged mount; a host without its daemon idles.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Engine`] when the engine cannot start it.
    fn run_container(
        &mut self,
        alias: &str,
        image: &str,
        command: &[&str],
    ) -> Result<(), FixtureError> {
        let name = format!("{}-{alias}", self.prefix);
        let owner = format!("{OWNER_LABEL}={}", std::process::id());
        let run_label = format!("{RUN_LABEL}={}", self.prefix);
        let timeout = self.options.container_timeout.as_secs().to_string();
        let mount = format!(
            "type=bind,source={},destination={MOUNT_POINT},ro=true",
            self.options.staged.display()
        );
        let mut arguments = vec![
            "run",
            "--detach",
            // A first process that reaps. Without one, every process orphaned
            // into a container stays in its table for ever: a shell that
            // backgrounds a command and exits leaves it to the first process,
            // and the first process here would otherwise be a `sleep`, which
            // never waits. Nothing shows for minutes; over hours the entries
            // accumulate until the container cannot fork at all, and every
            // command in it fails at once. A six-hour soak found this after
            // four of them, with two thousand orphaned `ssh` control masters
            // in the engine.
            "--init",
            "--name",
            &name,
            "--hostname",
            alias,
            "--network",
            &self.network,
            "--label",
            &run_label,
            "--label",
            &owner,
            "--timeout",
            &timeout,
            "--mount",
            &mount,
        ];
        if alias != ENGINE_ALIAS && !self.options.host_daemon {
            arguments.extend(["--entrypoint", IDLE_PROGRAM]);
        }
        arguments.push(image);
        if alias == ENGINE_ALIAS {
            arguments.extend(command);
        } else if !self.options.host_daemon {
            arguments.push(IDLE_ARGUMENT);
        }
        // A container that starts is recorded so teardown removes it; one
        // that fails to start is covered by teardown's network removal, the
        // labels, its own timeout and the next start's reaper.
        podman(&arguments, ENGINE_DEADLINE)?;
        self.containers.push((alias.to_owned(), name));
        Ok(())
    }

    /// The container name behind an alias.
    ///
    /// # Errors
    ///
    /// [`FixtureError::UnknownContainer`] for an alias no container has.
    fn name_of(&self, alias: &str) -> Result<&str, FixtureError> {
        self.containers
            .iter()
            .find(|(known, _name)| known == alias)
            .map(|(_alias, name)| name.as_str())
            .ok_or_else(|| FixtureError::UnknownContainer {
                alias: alias.to_owned(),
            })
    }

    /// Runs a shell command in a container as a user, captured.
    ///
    /// # Errors
    ///
    /// [`FixtureError::UnknownContainer`] for an unknown alias,
    /// [`FixtureError::Engine`] when the command fails or exceeds the
    /// deadline.
    fn exec_as(
        &self,
        alias: &str,
        user: &str,
        command: &str,
        deadline: Duration,
    ) -> Result<Completed, FixtureError> {
        let name = self.name_of(alias)?;
        podman(
            &["exec", "--user", user, name, "sh", "-c", command],
            deadline,
        )
    }

    /// Runs a shell command in a container as the unprivileged user.
    ///
    /// # Errors
    ///
    /// [`FixtureError::UnknownContainer`] for an unknown alias,
    /// [`FixtureError::Engine`] when the command fails or exceeds the
    /// deadline.
    pub fn exec(
        &self,
        container: &str,
        command: &str,
        deadline: Duration,
    ) -> Result<Completed, FixtureError> {
        self.exec_as(container, USER, command, deadline)
    }

    /// Generates the credentials inside the containers and the engine's SSH
    /// configuration naming each host by its container name.
    ///
    /// # Errors
    ///
    /// [`FixtureError`] when a container refuses a step.
    fn provision(&self) -> Result<(), FixtureError> {
        let generate = format!(
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh && ssh-keygen -q -t ed25519 -N '' -f {IDENTITY} && cat {IDENTITY}.pub"
        );
        let public_key = text(&self.exec(ENGINE_ALIAS, &generate, ENGINE_DEADLINE)?);
        let public_key = public_key.trim();
        let mut configuration = String::new();
        for index in 0..self.options.hosts {
            let alias = Fixture::host_alias(index);
            let host_name = self.name_of(&alias)?;
            let host_keys = if self.options.host_daemon {
                "rm -f /etc/ssh/ssh_host_*_key* && ssh-keygen -A && kill -HUP 1"
            } else {
                "true"
            };
            let authorize = format!(
                "{host_keys} && mkdir -p /home/{USER}/.ssh && printf '%s\\n' '{public_key}' > /home/{USER}/.ssh/authorized_keys && chown -R {USER}:{USER} /home/{USER}/.ssh && chmod 700 /home/{USER}/.ssh && chmod 600 /home/{USER}/.ssh/authorized_keys"
            );
            self.exec_as(&alias, "root", &authorize, ENGINE_DEADLINE)?;
            // Writing to a String cannot fail.
            let _written = write!(
                configuration,
                "Host {alias}\n  HostName {host_name}\n  User {USER}\n  IdentityFile {IDENTITY}\n  IdentitiesOnly yes\n  StrictHostKeyChecking accept-new\n  ConnectTimeout {CONNECT_TIMEOUT_SECONDS}\n"
            );
        }
        let write =
            format!("printf '%s' '{configuration}' > ~/.ssh/config && chmod 600 ~/.ssh/config");
        self.exec(ENGINE_ALIAS, &write, ENGINE_DEADLINE)?;
        Ok(())
    }

    /// Waits until every host answers `ssh <alias> true` from the engine.
    ///
    /// # Errors
    ///
    /// [`FixtureError::SshNotReady`] naming the first host that does not
    /// answer within the cap.
    fn await_ssh(&self) -> Result<(), FixtureError> {
        for index in 0..self.options.hosts {
            let alias = Fixture::host_alias(index);
            let probe = format!("ssh -o BatchMode=yes {alias} true");
            let mut last_attempt = String::new();
            wait_until(
                self.options.ssh_ready_cap,
                self.options.ssh_ready_interval,
                &format!("{alias} answering over SSH"),
                || match self.exec(ENGINE_ALIAS, &probe, ENGINE_DEADLINE) {
                    Ok(_answered) => true,
                    Err(error) => {
                        last_attempt = error.to_string();
                        false
                    }
                },
            )
            .map_err(|source| FixtureError::SshNotReady {
                alias: alias.clone(),
                cap: self.options.ssh_ready_cap,
                last_attempt: last_attempt.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// The process id a fault names, read from its file when it must be.
    ///
    /// # Errors
    ///
    /// [`FixtureError::ProcessId`] when the file does not hold one,
    /// [`FixtureError`] as reading it gives it.
    fn process_id(&self, alias: &str, process: &Process) -> Result<u32, FixtureError> {
        match process {
            Process::Id(id) => Ok(*id),
            Process::IdFile(path) => {
                let read = format!("cat '{}'", path.display());
                let content = text(&self.exec(alias, &read, ENGINE_DEADLINE)?);
                content
                    .trim()
                    .parse()
                    .map_err(|_error| FixtureError::ProcessId {
                        path: path.clone(),
                        content: content.trim().to_owned(),
                    })
            }
        }
    }

    /// Injects a fault.
    ///
    /// # Errors
    ///
    /// [`FixtureError`] when the engine or the container refuses.
    pub fn fault(&self, fault: &Fault) -> Result<(), FixtureError> {
        match fault {
            Fault::DisconnectNetwork { container } => {
                let name = self.name_of(container)?;
                podman(
                    &["network", "disconnect", "--force", &self.network, name],
                    ENGINE_DEADLINE,
                )?;
            }
            Fault::ReconnectNetwork { container } => {
                let name = self.name_of(container)?;
                podman(
                    &["network", "connect", &self.network, name],
                    ENGINE_DEADLINE,
                )?;
            }
            Fault::PauseProcess { container, process } => {
                self.signal(container, process, "STOP")?;
            }
            Fault::ResumeProcess { container, process } => {
                self.signal(container, process, "CONT")?;
            }
            Fault::KillProcess { container, process } => {
                self.signal(container, process, "KILL")?;
            }
        }
        Ok(())
    }

    /// Sends a signal to a process inside a container.
    ///
    /// # Errors
    ///
    /// [`FixtureError`] when the id cannot be read or the signal refused.
    fn signal(&self, alias: &str, process: &Process, signal: &str) -> Result<(), FixtureError> {
        let id = self.process_id(alias, process)?;
        self.exec(alias, &format!("kill -{signal} {id}"), ENGINE_DEADLINE)
            .map(|_completed| ())
    }

    /// From `start` to every host answering over SSH.
    #[must_use]
    pub fn elapsed_start(&self) -> Duration {
        self.elapsed_start
    }

    /// The run's prefix, which every container and network name carries.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Removes the containers and the network; failures are ignored, since
    /// the labels and the timeout make an orphan impossible to keep.
    fn teardown(&mut self) {
        for (_alias, name) in self.containers.drain(..) {
            let _removed = podman(&["rm", "--force", "--time", "0", &name], ENGINE_DEADLINE);
        }
        let _removed = podman(
            &["network", "rm", "--force", "--time", "0", &self.network],
            ENGINE_DEADLINE,
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.teardown();
    }
}
