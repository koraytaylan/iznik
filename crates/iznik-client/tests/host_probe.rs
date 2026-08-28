//! What a host says, and what iznik makes of it.
//!
//! Every case here is a string. The probe asks one script and reads its answer
//! with a pure function, so an unwritable home, a machine iznik has no
//! artifact for, a host without `tic` and a server already installed are a
//! table rather than four hosts to arrange — and the one thing that is not a
//! string, that the probe is a single round trip, is counted through a runner
//! of this test's own.

use core::fmt::Write as _;
use core::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iznik_client::bootstrap::probe::{
    Architecture, HostProbe, InstalledServer, OperatingSystem, PROBE_SCRIPT, ProbeError,
    RunsRemotely, parse, probe,
};

/// The deadline these cases hand the probe; nothing here waits for anything.
const AT_ONCE: Duration = Duration::from_secs(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A host's answer, built from what each field says.
fn answer(
    system: &str,
    machine: &str,
    tic: &str,
    terminfo: &str,
    candidates: &[(&str, &str)],
    server: &str,
) -> String {
    let mut said = format!("system {system}\nmachine {machine}\ntic {tic}\nterminfo {terminfo}\n");
    for (path, writable) in candidates {
        // Writing to a `String` cannot fail.
        let _written = writeln!(said, "candidate {path} {writable}");
    }
    let _written = writeln!(said, "server {server}");
    said
}

/// The three candidates a host offers, in the order the script asks about
/// them, with the writability each of them reports.
fn three(data: &str, home: &str, runtime: &str) -> Vec<(&'static str, String)> {
    vec![
        ("/home/iznik/.local/share/iznik", data.to_owned()),
        ("/home/iznik/.local/share/iznik", home.to_owned()),
        ("/run/user/1000/iznik/iznik", runtime.to_owned()),
    ]
}

/// An ordinary Linux answer with the writability the arguments say.
fn linux(data: &str, home: &str, runtime: &str, server: &str) -> String {
    let offered = three(data, home, runtime);
    let borrowed: Vec<(&str, &str)> = offered
        .iter()
        .map(|(path, writable)| (*path, writable.as_str()))
        .collect();
    answer("Linux", "x86_64", "yes", "no", &borrowed, server)
}

/// A runner that counts how many commands it was asked to run and always says
/// the same thing.
#[derive(Clone)]
struct Counted {
    /// What it answers with.
    said: String,
    /// How many times it has been asked.
    asked: Arc<AtomicUsize>,
}

impl RunsRemotely for Counted {
    fn run(
        &self,
        command: &str,
        _deadline: Duration,
    ) -> impl Future<Output = Result<String, ProbeError>> + Send {
        self.asked.fetch_add(1, Ordering::Relaxed);
        let said = self.said.clone();
        let script = command.to_owned();
        async move {
            if script.trim() == PROBE_SCRIPT.trim() {
                Ok(said)
            } else {
                Err(ProbeError::Malformed {
                    detail: "that is not the probe's own script".to_owned(),
                })
            }
        }
    }
}

/// # Panics
///
/// When a plain Linux host is not read as one.
#[test]
fn host_probe_reads_an_ordinary_linux_host() {
    let read = parse(&linux("yes", "yes", "yes", "-")).expect("a probe");
    assert_eq!(
        read,
        HostProbe {
            operating_system: OperatingSystem::Linux,
            architecture: Architecture::X86_64,
            server: None,
            terminfo_installed: false,
            tic_available: true,
            prefix: PathBuf::from("/home/iznik/.local/share/iznik"),
        },
        "a fresh host with tic and no server"
    );
}

/// # Panics
///
/// When a machine iznik serves is not recognized, or one it does not serve is
/// not refused by name.
#[test]
fn host_probe_knows_the_machines_it_serves() {
    let case = |system: &str, machine: &str| {
        let offered = three("yes", "yes", "yes");
        let borrowed: Vec<(&str, &str)> = offered
            .iter()
            .map(|(path, writable)| (*path, writable.as_str()))
            .collect();
        parse(&answer(system, machine, "yes", "no", &borrowed, "-"))
    };
    for (system, machine, wanted_system, wanted_machine) in [
        (
            "Linux",
            "x86_64",
            OperatingSystem::Linux,
            Architecture::X86_64,
        ),
        (
            "Linux",
            "amd64",
            OperatingSystem::Linux,
            Architecture::X86_64,
        ),
        (
            "Linux",
            "aarch64",
            OperatingSystem::Linux,
            Architecture::Aarch64,
        ),
        (
            "Darwin",
            "arm64",
            OperatingSystem::Darwin,
            Architecture::Aarch64,
        ),
        (
            "Darwin",
            "x86_64",
            OperatingSystem::Darwin,
            Architecture::X86_64,
        ),
    ] {
        let read = case(system, machine).expect("a machine iznik serves");
        assert_eq!(read.operating_system, wanted_system, "{system} {machine}");
        assert_eq!(read.architecture, wanted_machine, "{system} {machine}");
    }
    for (system, machine) in [("SunOS", "sun4v"), ("Linux", "riscv64"), ("Plan9", "386")] {
        let refused = case(system, machine);
        let Err(ProbeError::Unsupported {
            operating_system,
            architecture,
        }) = refused
        else {
            panic!("{system} {machine} was not refused: {refused:?}");
        };
        assert_eq!(operating_system, system, "and says what it runs");
        assert_eq!(architecture, machine, "and what it is");
    }
}

/// # Panics
///
/// When the prefix is not the first candidate the host will let iznik write,
/// or nowhere writable is not said so.
#[test]
fn host_probe_takes_the_first_prefix_the_host_allows() {
    let chosen = |data: &str, home: &str, runtime: &str| {
        parse(&linux(data, home, runtime, "-")).map(|read| read.prefix)
    };
    assert_eq!(
        chosen("yes", "yes", "yes").expect("a prefix"),
        PathBuf::from("/home/iznik/.local/share/iznik"),
        "the data home when it is writable"
    );
    assert_eq!(
        chosen("no", "yes", "yes").expect("a prefix"),
        PathBuf::from("/home/iznik/.local/share/iznik"),
        "the home's share when it is not"
    );
    assert_eq!(
        chosen("no", "no", "yes").expect("a prefix"),
        PathBuf::from("/run/user/1000/iznik/iznik"),
        "and the runtime directory when neither is"
    );
    let nowhere = chosen("no", "no", "no");
    let Err(ProbeError::Unwritable { candidates }) = nowhere else {
        panic!("a host that allows nothing was not refused: {nowhere:?}");
    };
    assert_eq!(candidates.len(), 3, "and names every place it tried");
}

/// # Panics
///
/// When an installed server is not read, or a version line this cannot read is
/// taken for one.
#[test]
fn host_probe_reads_an_installed_server() {
    let read = parse(&linux("yes", "yes", "yes", "iznik-server 0.1.0 protocol 1"))
        .expect("a probe")
        .server;
    assert_eq!(
        read,
        Some(InstalledServer {
            crate_version: "0.1.0".to_owned(),
            protocol_version: 1,
        }),
        "the version and the protocol it speaks"
    );
    for said in ["-", "iznik-server 0.1.0", "something else entirely", ""] {
        assert_eq!(
            parse(&linux("yes", "yes", "yes", said))
                .expect("a probe")
                .server,
            None,
            "and nothing where there is nothing to read: {said:?}"
        );
    }
}

/// # Panics
///
/// When `tic` and the terminfo are not read as the host reported them.
#[test]
fn host_probe_reads_what_the_terminal_needs() {
    for (tic, terminfo) in [("yes", "yes"), ("yes", "no"), ("no", "no"), ("no", "yes")] {
        let offered = three("yes", "yes", "yes");
        let borrowed: Vec<(&str, &str)> = offered
            .iter()
            .map(|(path, writable)| (*path, writable.as_str()))
            .collect();
        let read =
            parse(&answer("Linux", "x86_64", tic, terminfo, &borrowed, "-")).expect("a probe");
        assert_eq!(read.tic_available, tic == "yes", "tic {tic}");
        assert_eq!(
            read.terminfo_installed,
            terminfo == "yes",
            "terminfo {terminfo}"
        );
    }
}

/// # Panics
///
/// When an answer missing a field it needs is read as though it were whole.
#[test]
fn host_probe_refuses_an_answer_it_cannot_read() {
    for (said, missing) in [
        ("machine x86_64\ncandidate /a yes\n", "system"),
        ("system Linux\ncandidate /a yes\n", "machine"),
        ("system Linux\nmachine x86_64\n", "candidate"),
    ] {
        let refused = parse(said);
        let Err(ProbeError::Malformed { detail }) = refused else {
            panic!("an answer without `{missing}` was read anyway: {refused:?}");
        };
        assert!(
            detail.contains(missing),
            "and says what was missing: {detail}"
        );
    }
}

/// # Panics
///
/// When the probe is more than one round trip, or does not run its own script.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_probe_is_one_round_trip() {
    let case = async {
        let asked = Arc::new(AtomicUsize::new(0));
        let runner = Counted {
            said: linux("yes", "yes", "yes", "-"),
            asked: Arc::clone(&asked),
        };
        let read = probe(&runner, AT_ONCE).await?;
        assert_eq!(
            read.prefix,
            Path::new("/home/iznik/.local/share/iznik"),
            "the probe read what the host said"
        );
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "and asked exactly once, because latency to a distant host is the cost"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
