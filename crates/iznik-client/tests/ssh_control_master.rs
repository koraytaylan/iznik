//! The `ssh` transport's pure parts: what iznik puts on a command line, what
//! it does not, where a control socket goes, and what `ssh`'s own words mean.
//!
//! Nothing here starts a process. What the argument vector *lacks* is the
//! property that matters — every option a person could have written in
//! `~/.ssh/config` — and a vector is a value. The classification is the same:
//! `ssh`'s output for a refusal, an unresolvable name, a denied key and a
//! changed host key are captured under `tests/fixtures/ssh/`, so five hosts do
//! not have to be arranged to establish what five messages mean. The container
//! scenarios arrange them anyway, against the real thing.

use std::path::{Path, PathBuf};

use iznik_client::transport::ssh::{
    CONNECT_TIMEOUT, CONTROL_PERSIST, SERVER_ALIVE_COUNT_MAXIMUM, SERVER_ALIVE_INTERVAL, SshError,
    SshOptions, classify,
};
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX, Transport};

/// The longest a unix socket path may be, on the platform that allows the
/// least: macOS, where `sockaddr_un.sun_path` is 104 bytes including its
/// terminator. A control path longer than this is one `ssh` cannot bind.
const SOCKET_PATH_LIMIT: usize = 104;

/// Every option a person may already have written in their own configuration,
/// and every short flag that means the same thing. None may appear.
const NEVER_PASSED: &[&str] = &[
    "User=",
    "Port=",
    "IdentityFile=",
    "ProxyJump=",
    "HostName=",
    "-l",
    "-p",
    "-i",
    "-J",
];

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A temporary directory of this test's own, removed when the guard drops.
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When the directory cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let path = std::env::temp_dir().join(format!("iznik-ssh-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// The captured output of one `ssh` failure.
///
/// # Errors
///
/// When the fixture cannot be read.
fn captured(name: &str) -> Result<String, Failed> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ssh")
        .join(format!("{name}.txt"));
    Ok(std::fs::read_to_string(path)?)
}

/// # Panics
///
/// When the argument vector is missing an option iznik owns, or carries one a
/// person could have configured.
#[test]
fn ssh_passes_what_it_owns_and_nothing_a_person_configured() {
    let case = || -> Result<(), Failed> {
        let held = scratch("arguments")?;
        let paths = ClientRuntimePaths::under(&held.path)?;
        let options = SshOptions::default();
        let Transport::Ssh(transport) = Transport::for_alias("host0", &paths, options) else {
            return Err("an ordinary alias is not local".into());
        };
        let arguments = transport.arguments(&["/iznik/bin/iznik-server".to_owned()]);
        let line = arguments.join(" ");
        for wanted in [
            "ControlMaster=auto".to_owned(),
            format!("ControlPath={}", transport.control_path().display()),
            format!("ControlPersist={}", CONTROL_PERSIST.as_secs()),
            format!("ServerAliveInterval={}", SERVER_ALIVE_INTERVAL.as_secs()),
            format!("ServerAliveCountMax={SERVER_ALIVE_COUNT_MAXIMUM}"),
            format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()),
        ] {
            assert!(
                arguments.contains(&wanted),
                "the options iznik owns include {wanted}: {line}"
            );
        }
        for never in NEVER_PASSED {
            assert!(
                !line.contains(never),
                "and none a person could have configured: {never} is in {line}"
            );
        }
        let alias = arguments
            .iter()
            .position(|argument| argument == "host0")
            .ok_or("the alias is not on the command line")?;
        assert_eq!(
            arguments.get(alias.saturating_add(1)).map(String::as_str),
            Some("/iznik/bin/iznik-server"),
            "and the command follows the alias: {line}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a `unix:` alias is not local, an ordinary one is not `ssh`, or an
/// alias does not survive the round trip.
#[test]
fn ssh_reads_the_one_alias_form_it_owns() {
    let case = || -> Result<(), Failed> {
        let held = scratch("alias")?;
        let paths = ClientRuntimePaths::under(&held.path)?;
        let socket = "/run/iznik/server.sock";
        let local = Transport::for_alias(
            &format!("{LOCAL_PREFIX}{socket}"),
            &paths,
            SshOptions::default(),
        );
        let Transport::Local { socket: named } = &local else {
            return Err("a `unix:` alias is not local".into());
        };
        assert_eq!(named, Path::new(socket), "and it names the socket it gave");
        assert_eq!(
            local.alias(),
            format!("{LOCAL_PREFIX}{socket}"),
            "and says back what it was given"
        );
        for alias in ["host0", "user@example.com", "bastion-then-host"] {
            let transport = Transport::for_alias(alias, &paths, SshOptions::default());
            assert!(
                matches!(transport, Transport::Ssh(_)),
                "{alias} is handed to ssh"
            );
            assert_eq!(transport.alias(), alias, "untouched");
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When two aliases share a control path, or one is too long to bind.
#[test]
fn ssh_names_a_control_socket_that_fits() {
    let case = || -> Result<(), Failed> {
        let held = scratch("control")?;
        let paths = ClientRuntimePaths::under(&held.path)?;
        // The second is as long as an alias gets: a jump chain written out.
        let aliases = [
            "host0",
            "host1",
            "deploy@bastion.example.com,deploy@build-07.internal.example.com",
        ];
        let mut seen: Vec<PathBuf> = Vec::new();
        for alias in aliases {
            let path = paths.control_path(alias);
            assert!(
                path.as_os_str().len() < SOCKET_PATH_LIMIT,
                "a control path must fit in {SOCKET_PATH_LIMIT} bytes: {} is {}",
                path.display(),
                path.as_os_str().len()
            );
            assert!(!seen.contains(&path), "and be its own: {}", path.display());
            seen.push(path);
        }
        assert_eq!(
            paths.control_path(aliases[0]),
            paths.control_path(aliases[0]),
            "and be the same one every time, or a master is never reused"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a captured `ssh` failure is classified as something other than what it
/// is, or does not carry the host and the words a person needs.
#[test]
fn ssh_says_which_failure_it_was() {
    let case = || -> Result<(), Failed> {
        let host = "host0";
        let unreachable = ["refused", "unresolvable", "timed-out"];
        for name in unreachable {
            let said = captured(name)?;
            let classified = classify(host, Some(255), &said);
            let SshError::Unreachable {
                host: named,
                detail,
            } = &classified
            else {
                return Err(format!("{name} is not unreachable: {classified:?}").into());
            };
            assert_eq!(named, host, "and names the host");
            assert!(!detail.is_empty(), "and says what ssh said");
        }
        let denied = classify(host, Some(255), &captured("denied")?);
        assert!(
            matches!(denied, SshError::AuthenticationFailed { .. }),
            "a denied key is authentication: {denied:?}"
        );
        let changed = classify(host, Some(255), &captured("host-key-changed")?);
        let SshError::HostKeyChanged { .. } = &changed else {
            return Err(format!("a changed host key is its own thing: {changed:?}").into());
        };
        let alarming = changed.to_string();
        for wanted in ["host key", host, "do not connect"] {
            assert!(
                alarming.contains(wanted),
                "and says so alarmingly: {alarming}"
            );
        }
        let failed = classify(host, Some(127), &captured("command-failed")?);
        let SshError::RemoteCommandFailed { status, stderr, .. } = &failed else {
            return Err(format!("a command that failed is not the link: {failed:?}").into());
        };
        assert_eq!(*status, 127, "and carries what it exited with");
        assert!(
            stderr.contains("No such file"),
            "and what it said: {stderr}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When an `ssh` that said nothing recognizable is taken for a command that
/// failed, or a command that failed is taken for a link that did.
#[test]
fn ssh_tells_a_link_failure_from_a_command_failure() {
    let unknown = classify("host0", Some(255), "ssh: something nobody has seen\n");
    assert!(
        matches!(unknown, SshError::Unreachable { .. }),
        "255 is ssh's own status, whatever it said: {unknown:?}"
    );
    let killed = classify("host0", None, "");
    assert!(
        matches!(killed, SshError::Unreachable { .. }),
        "and so is no status at all: {killed:?}"
    );
    let failed = classify("host0", Some(1), "true failed somehow\n");
    assert!(
        matches!(failed, SshError::RemoteCommandFailed { status: 1, .. }),
        "while any other status is the command's: {failed:?}"
    );
}
