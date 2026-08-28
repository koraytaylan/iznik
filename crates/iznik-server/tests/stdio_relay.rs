//! The one command a bootstrap ever runs on a host, held to what it promises:
//! it finds the daemon or starts it, carries bytes both ways untouched, ends
//! when either side does, and says so audibly when it cannot.
//!
//! Every case runs the real binary and speaks the real protocol through its
//! standard streams, because standard streams are the whole of what it is.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::message::PROTOCOL_VERSION;
use iznik_testkit::client::{Received, TestClient};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// The binary every case runs.
const SERVER: &str = env!("CARGO_BIN_EXE_iznik-server");

/// How long a case waits for something the relay should do at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long a case gives the relay to notice its client has gone.
const PARTING: Duration = Duration::from_secs(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A runtime directory of a case's own, and whatever daemon is holding it.
#[derive(Debug)]
struct Home {
    /// Where it is.
    path: PathBuf,
}

impl Home {
    /// A directory named for the case that asked for it.
    ///
    /// # Errors
    ///
    /// When it cannot be created.
    fn new(named: &str) -> Result<Home, Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let path = base.join(format!("iznik-relay-{named}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Home { path })
    }

    /// The socket the daemon will listen on.
    fn socket(&self) -> PathBuf {
        self.path.join("iznik").join("server.sock")
    }

    /// The lock it will hold.
    fn lock(&self) -> PathBuf {
        self.path.join("iznik").join("server.lock")
    }

    /// The server, with this directory as its runtime base.
    fn server(&self) -> Command {
        let mut command = Command::new(SERVER);
        command.env("XDG_RUNTIME_DIR", &self.path);
        command
    }

    /// A relay child with its standard streams on pipes.
    ///
    /// # Errors
    ///
    /// When it cannot be spawned.
    fn relay(&self) -> Result<Child, Failed> {
        Ok(self
            .server()
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let iznik_server::daemon::lock::Holder::Held { process_id } =
            iznik_server::daemon::lock::held_by(&self.lock())
            && let Ok(pid) = i32::try_from(process_id)
        {
            let _told = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A child's standard streams as one duplex thing, which is what a client
/// speaks over.
#[derive(Debug)]
struct Pipes {
    /// What goes to the relay.
    input: ChildStdin,
    /// What comes back.
    output: ChildStdout,
}

impl AsyncRead for Pipes {
    fn poll_read(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        core::pin::Pin::new(&mut self.get_mut().output).poll_read(context, buffer)
    }
}

impl AsyncWrite for Pipes {
    fn poll_write(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
        bytes: &[u8],
    ) -> core::task::Poll<std::io::Result<usize>> {
        core::pin::Pin::new(&mut self.get_mut().input).poll_write(context, bytes)
    }

    fn poll_flush(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        core::pin::Pin::new(&mut self.get_mut().input).poll_flush(context)
    }

    fn poll_shutdown(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        core::pin::Pin::new(&mut self.get_mut().input).poll_shutdown(context)
    }
}

/// The relay's streams as a client speaks over them.
///
/// # Errors
///
/// When the child has no pipes, which only a spawn without them does.
fn speaking(child: &mut Child) -> Result<TestClient<Pipes>, Failed> {
    let input = child
        .stdin
        .take()
        .ok_or("the relay has no standard input")?;
    let output = child
        .stdout
        .take()
        .ok_or("the relay has no standard output")?;
    Ok(TestClient::over(Pipes { input, output }))
}

/// # Panics
///
/// When a relay against a running daemon does not carry a handshake and a
/// liveness round trip through untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_carries_the_protocol_to_a_running_daemon() {
    let case = async {
        let home = Home::new("running")?;
        let started = home
            .server()
            .arg("--daemon")
            .arg("--idle-shutdown-seconds")
            .arg("120")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        assert!(started.success(), "the daemon starts");

        let mut child = home.relay()?;
        let mut client = speaking(&mut child)?;
        let greeting = client.hello(Capabilities::from_bits(0)).await?;
        assert_eq!(
            greeting.protocol_version, PROTOCOL_VERSION,
            "the handshake goes through untouched"
        );
        client.ping().await?;
        let pong = client.next(PROMPT).await?;
        assert!(
            matches!(
                pong,
                Received::Control(iznik_protocol::message::ToClient::Pong)
            ),
            "and so does what follows it: {pong:?}"
        );
        let _told = child.start_kill();
        let _waited = child.wait().await;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a relay with no daemon behind it does not start one, or the daemon it
/// starts does not outlive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_starts_the_daemon_and_the_daemon_outlives_it() {
    let case = async {
        let home = Home::new("first-use")?;
        assert!(!home.socket().exists(), "nothing is running yet");

        let mut child = home.relay()?;
        let mut client = speaking(&mut child)?;
        let greeting = client.hello(Capabilities::from_bits(0)).await?;
        assert_eq!(
            greeting.protocol_version, PROTOCOL_VERSION,
            "the relay started one and shook hands through it"
        );
        let socket = home.socket();
        assert!(socket.exists(), "and its socket is there");

        drop(client);
        let _told = child.start_kill();
        let _waited = child.wait().await;
        tokio::time::sleep(PARTING).await;
        assert!(
            tokio::net::UnixStream::connect(&socket).await.is_ok(),
            "the daemon outlives the relay that started it"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a relay whose client has gone stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_ends_when_its_client_goes() {
    let case = async {
        let home = Home::new("closed-input")?;
        let mut child = home.relay()?;
        let mut client = speaking(&mut child)?;
        let _greeting = client.hello(Capabilities::from_bits(0)).await?;
        // The client goes: its end of the pipe closes, and the relay has
        // nothing left to carry.
        drop(client);
        let ended = tokio::time::timeout(PROMPT, child.wait()).await??;
        assert!(ended.success(), "it exits cleanly when its client goes");
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a relay whose daemon has gone reports a failure rather than an
/// ending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_ends_when_the_daemon_goes() {
    let case = async {
        let home = Home::new("closed-socket")?;
        let mut child = home.relay()?;
        let mut client = speaking(&mut child)?;
        let _greeting = client.hello(Capabilities::from_bits(0)).await?;
        let stopped = home
            .server()
            .arg("--stop")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        assert!(stopped.success(), "the daemon is stopped under it");
        let ended = tokio::time::timeout(PROMPT, child.wait()).await??;
        assert!(ended.success(), "and it exits cleanly when the daemon goes");
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a relay that cannot make a runtime directory fails silently, or fails
/// without saying which path it could not use.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_fails_audibly_when_it_cannot_make_its_directory() {
    let case = async {
        let home = Home::new("no-directory")?;
        // A file where a directory would have to be: nothing can be created
        // under it, which is what a locked-down host looks like from here.
        let blocked = home.path.join("a-file");
        std::fs::write(&blocked, b"not a directory")?;
        let refused = Command::new(SERVER)
            .env("XDG_RUNTIME_DIR", &blocked)
            .arg("--stdio")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        assert!(!refused.status.success(), "it fails rather than hanging");
        let said = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(
            said.contains(&blocked.display().to_string()),
            "and names the path it could not use: {said}"
        );
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
