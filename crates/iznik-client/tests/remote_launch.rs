//! What a bootstrap decides, and what it says when the far end will not agree.
//!
//! The decision is a pure function of what a host answered and what this build
//! carries, so every case about it is two strings and a directory — and the
//! one that matters most, that a machine iznik has no artifact for is told so
//! *before* anything is sent, is provable only because nothing in it touches a
//! host. What needs a server — the stage a refusal names — is a scripted one
//! over a socket of this case's own; what needs a real host is a scenario.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iznik_client::bootstrap::launch::{
    BootstrapOptions, Decision, Stage, bundled, decide, expiry, launch, live_panes, server_path,
    triple_of,
};
use iznik_client::bootstrap::probe::{Architecture, HostProbe, InstalledServer, OperatingSystem};
use iznik_client::bootstrap::upload::{ArtifactSet, BINARY_NAME};
use iznik_client::transport::channel::ChannelOptions;
use iznik_client::transport::ssh::SshOptions;
use iznik_client::transport::{ClientRuntimePaths, LOCAL_PREFIX, Transport};
use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::message::{
    CHANNEL_CONTROL, ErrorCode, PROTOCOL_VERSION, ToClient, decode_to_server, encode_to_client,
};
use iznik_testkit::stack::{Stack, StackOptions};
use tokio::net::UnixListener;

/// The prefix a probed host chose.
const PREFIX: &str = "/home/iznik/.local/share/iznik";

/// The triple this machine's host answers as.
const TRIPLE: &str = "x86_64-unknown-linux-musl";

/// A version no server speaks, for the mismatch case.
const OTHER_VERSION: u16 = PROTOCOL_VERSION.saturating_add(1);

/// A version no build of this carries, for the upgrade case.
const OLDER_VERSION: &str = "0.0.1";

/// How long these cases give a server that should answer at once.
const PROMPT: Duration = Duration::from_secs(5);

/// How long they let a scripted server hold its peace before giving up on it.
const BRIEF: Duration = Duration::from_millis(400);

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

/// A scratch directory named for `case`, holding one artifact per triple.
///
/// # Errors
///
/// When it cannot be made or written.
fn scratch(case: &str, triples: &[&str]) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-launch-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    for triple in triples {
        let held = path.join(triple);
        std::fs::create_dir_all(&held)?;
        std::fs::write(held.join(BINARY_NAME), b"a server")?;
    }
    Ok(Scratch { path })
}

/// A probed Linux host with the server the argument says.
fn probed(server: Option<InstalledServer>) -> HostProbe {
    HostProbe {
        operating_system: OperatingSystem::Linux,
        architecture: Architecture::X86_64,
        server,
        terminfo_installed: true,
        tic_available: true,
        prefix: PathBuf::from(PREFIX),
    }
}

/// The transport that reaches `socket`, with this case's own runtime paths.
///
/// # Errors
///
/// When the runtime paths cannot be made.
fn local(held: &Scratch, socket: &Path) -> Result<Transport, Failed> {
    let paths = ClientRuntimePaths::under(&held.path.join("runtime"))?;
    Ok(Transport::for_alias(
        &format!("{LOCAL_PREFIX}{}", socket.display()),
        &paths,
        SshOptions::default(),
    ))
}

/// Options that give up quickly, because these cases are about what is said,
/// not about how long anything waits.
fn brisk() -> BootstrapOptions {
    BootstrapOptions {
        snapshot_deadline: BRIEF,
        channel: ChannelOptions {
            open_deadline: BRIEF,
            ..ChannelOptions::default()
        },
        ..BootstrapOptions::default()
    }
}

/// Answers one connection with whatever `answer` says, and then goes.
///
/// The listener is bound before this returns, so a caller may connect at once.
///
/// # Errors
///
/// When the socket cannot be bound.
fn scripted(socket: &Path, answer: ToClient) -> Result<tokio::task::JoinHandle<()>, Failed> {
    let listener = UnixListener::bind(socket)?;
    Ok(tokio::spawn(async move {
        let Ok((stream, _from)) = listener.accept().await else {
            return;
        };
        let mut link = FramedLink::new(stream);
        let Ok(Some(frame)) = link.next_frame().await else {
            return;
        };
        if decode_to_server(frame.payload).is_err() {
            return;
        }
        let Ok(said) = encode_to_client(&answer) else {
            return;
        };
        let _sent = link.send(CHANNEL_CONTROL, &said).await;
        // Held open, so what the case sees is the refusal and not a link that
        // went before it could be read.
        tokio::time::sleep(PROMPT).await;
    }))
}

/// # Panics
///
/// When a host with no server is not one to install on.
#[test]
fn remote_launch_installs_where_there_is_no_server() {
    let case = || -> Result<(), Failed> {
        let held = scratch("install", &[TRIPLE])?;
        let artifacts = ArtifactSet::load(&held.path)?;
        assert_eq!(
            decide(&probed(None), &artifacts, &bundled()),
            Decision::Install,
            "a host with nothing on it is installed on"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a host that already has this build's server is uploaded to anyway.
#[test]
fn remote_launch_is_up_to_date_when_the_versions_match() {
    let case = || -> Result<(), Failed> {
        let held = scratch("current", &[TRIPLE])?;
        let artifacts = ArtifactSet::load(&held.path)?;
        // Nothing is uploaded in this case, and nothing being uploaded is what
        // makes every connection after the first one fast.
        assert_eq!(
            decide(&probed(Some(bundled())), &artifacts, &bundled()),
            Decision::UpToDate,
            "a matching version is left alone"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When another version on the host is replaced rather than offered.
#[test]
fn remote_launch_offers_an_upgrade_for_another_version() {
    let case = || -> Result<(), Failed> {
        let held = scratch("upgrade", &[TRIPLE])?;
        let artifacts = ArtifactSet::load(&held.path)?;
        let older = InstalledServer {
            crate_version: OLDER_VERSION.to_owned(),
            protocol_version: PROTOCOL_VERSION,
        };
        // The daemon *is* the sessions: what an older one gets is an offer,
        // never a replacement nobody asked for.
        assert_eq!(
            decide(&probed(Some(older.clone())), &artifacts, &bundled()),
            Decision::UpgradeAvailable {
                installed: older,
                bundled: bundled(),
            },
            "both versions are carried, so somebody can be asked"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a machine this build carries no server for is discovered by trying,
/// rather than said before anything is sent.
#[test]
fn remote_launch_refuses_a_machine_it_carries_nothing_for() {
    let case = || -> Result<(), Failed> {
        // A set with an artifact for the other architecture only.
        let held = scratch("unsupported", &["aarch64-apple-darwin"])?;
        let artifacts = ArtifactSet::load(&held.path)?;
        let found = probed(None);
        assert_eq!(
            decide(&found, &artifacts, &bundled()),
            Decision::Unsupported {
                triple: TRIPLE.to_owned(),
            },
            "it names the triple that would have been needed"
        );
        assert_eq!(triple_of(&found), TRIPLE, "which is the host's own");
        assert_eq!(
            server_path(&found),
            PathBuf::from(PREFIX).join("bin").join(BINARY_NAME),
            "and the server it would have run is under the prefix the probe chose"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a server that speaks another protocol version is not refused at the
/// handshake, naming the version it said.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_launch_names_the_handshake_when_the_server_disagrees() {
    let case = async {
        let held = scratch("disagrees", &[])?;
        let socket = held.path.join("other-version.sock");
        let answering = scripted(
            &socket,
            ToClient::Hello {
                protocol_version: OTHER_VERSION,
                server_version: "scripted".to_owned(),
                capabilities: Capabilities::from_bits(0),
            },
        )?;
        let transport = local(&held, &socket)?;
        let refused = launch(&transport, None, &brisk(), expiry(PROMPT)).await;
        answering.abort();
        let Err(error) = refused else {
            return Err("a server speaking another version was accepted".into());
        };
        assert_eq!(
            error.stage,
            Stage::Handshake,
            "the stage is the handshake, which is where it went wrong: {error}"
        );
        assert!(
            error.detail.contains(&format!("iznik/{OTHER_VERSION}")),
            "and it says which version the server speaks: {error}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a server that answers the handshake with a refusal is not reported at
/// the handshake, carrying what it said.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_launch_names_the_handshake_when_the_server_refuses() {
    let case = async {
        let held = scratch("refuses", &[])?;
        let socket = held.path.join("refusing.sock");
        let answering = scripted(
            &socket,
            ToClient::Error {
                code: ErrorCode::ProtocolVersion,
                message: format!("this server speaks iznik/{OTHER_VERSION}"),
            },
        )?;
        let transport = local(&held, &socket)?;
        let refused = launch(&transport, None, &brisk(), expiry(PROMPT)).await;
        answering.abort();
        let Err(error) = refused else {
            return Err("a server that refused the handshake was accepted".into());
        };
        assert_eq!(
            error.stage,
            Stage::Handshake,
            "the stage is the handshake: {error}"
        );
        assert!(
            error.detail.contains("ProtocolVersion"),
            "and the remote's own words are carried: {error}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// Accepts one connection and says nothing at all, which is what a server that
/// started and never greeted looks like.
///
/// # Errors
///
/// When the socket cannot be bound.
fn silent(socket: &Path) -> Result<tokio::task::JoinHandle<()>, Failed> {
    let listener = UnixListener::bind(socket)?;
    Ok(tokio::spawn(async move {
        let Ok((stream, _from)) = listener.accept().await else {
            return;
        };
        // Held, because a socket that closed would be a link that went rather
        // than a greeting that never came.
        tokio::time::sleep(PROMPT).await;
        drop(stream);
    }))
}

/// # Panics
///
/// When a server that opened a link and then said nothing is not reported at
/// the handshake.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_launch_names_the_handshake_when_the_server_says_nothing() {
    let case = async {
        let held = scratch("silent", &[])?;
        let socket = held.path.join("mute.sock");
        let answering = silent(&socket)?;
        let transport = local(&held, &socket)?;
        let started = Instant::now();
        let refused = launch(&transport, None, &brisk(), expiry(PROMPT)).await;
        let taken = started.elapsed();
        answering.abort();
        let Err(error) = refused else {
            return Err("a server that never greeted was accepted".into());
        };
        // The link was made and time was given; what did not come is the
        // greeting. Calling this a launch would send somebody to look at a
        // path for a server that started perfectly well.
        assert_eq!(
            error.stage,
            Stage::Handshake,
            "silence after a link opens is the handshake: {error}"
        );
        assert!(
            taken < PROMPT,
            "and it says so inside the deadline it was given: {taken:?}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a budget already spent is reported as a server that would not answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_launch_refuses_a_budget_already_spent() {
    let case = async {
        let held = scratch("spent", &[])?;
        let socket = held.path.join("never.sock");
        let transport = local(&held, &socket)?;
        // Every stage takes the lesser of its own deadline and what remains of
        // the whole budget. With nothing remaining, nothing is attempted — and
        // saying the handshake failed would be a refusal about a server that
        // was never started.
        let refused = launch(&transport, None, &brisk(), Instant::now()).await;
        let Err(error) = refused else {
            return Err("a bootstrap with no budget left opened a channel".into());
        };
        assert_eq!(
            error.stage,
            Stage::Launch,
            "the stage is the launch: {error}"
        );
        assert!(
            error.detail.contains("budget"),
            "and it says the budget was spent: {error}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a daemon that is there does not hand back a snapshot to launch from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_launch_takes_a_snapshot_from_the_daemon() {
    let case = async {
        let held = scratch("snapshot", &[])?;
        let stack = Stack::start(StackOptions::default()).await?;
        let transport = local(&held, stack.socket())?;
        let (channel, snapshot) = launch(&transport, None, &brisk(), expiry(PROMPT)).await?;
        assert_eq!(
            live_panes(&snapshot),
            0,
            "a daemon nobody has asked for anything holds no panes"
        );
        assert_eq!(
            snapshot.sessions.len(),
            0,
            "and no sessions: {:?}",
            snapshot.sessions
        );
        channel.close();
        drop(stack);
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the whole budget is not the ceiling every stage runs under.
#[test]
fn remote_launch_gives_each_stage_what_is_left_of_the_budget() {
    // The budget a caller hands a bootstrap is the whole of it, and a stage
    // whose own deadline is longer than what remains gets what remains — which
    // is how a refusal names the stage it happened in rather than a timeout
    // that could have been any of them.
    let expires = expiry(Duration::from_millis(50));
    let stage = iznik_client::bootstrap::launch::left(expires, Duration::from_secs(30));
    assert!(
        stage <= Duration::from_millis(50),
        "no stage outlasts the budget: {stage:?}"
    );
    let long = expiry(Duration::from_mins(10));
    let capped = iznik_client::bootstrap::launch::left(long, Duration::from_millis(20));
    assert!(
        capped <= Duration::from_millis(20),
        "and none outlasts its own deadline either: {capped:?}"
    );
    let past = Instant::now();
    assert_eq!(
        iznik_client::bootstrap::launch::left(past, Duration::from_secs(1)),
        Duration::ZERO,
        "and a budget already spent leaves nothing"
    );
}
