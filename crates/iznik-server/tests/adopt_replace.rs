//! A real daemon replaced in place. The child is a real shell, and the bytes
//! after the replacement are bytes that shell wrote.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::identity::{DaemonInstance, PaneId};
use iznik_testkit::client::TestClient;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::Command;

/// The server this crate just built.
const SERVER: &str = env!("CARGO_BIN_EXE_iznik-server");

/// How long a step that should happen at once may take.
const PROMPT: Duration = Duration::from_secs(20);

/// How long between looks.
const POLL: Duration = Duration::from_millis(50);

/// How long one read from the server may take.
const READ: Duration = Duration::from_secs(5);

/// The pane a new session mints first.
const PANE: PaneId = PaneId(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A private runtime directory, removed with the daemon when dropped.
struct Home {
    /// Where it is.
    path: PathBuf,
    /// The daemon, when one was started.
    child: Option<tokio::process::Child>,
}

impl Home {
    /// A directory of this case's own.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created.
    fn new(named: &str) -> Result<Home, Failed> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let path = base.join(format!("iznik-adopt-{named}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Home { path, child: None })
    }

    /// The socket.
    fn socket(&self) -> PathBuf {
        self.path.join("iznik").join("server.sock")
    }

    /// The adoption record's directory, where a refusal marker is written.
    fn runtime(&self) -> PathBuf {
        self.path.join("iznik")
    }

    /// A copy of the server, so the test execs that copy and not the build tree.
    fn binary(&self) -> PathBuf {
        self.path.join("iznik-server")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _killed = child.start_kill();
        }
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// Waits until `looking` holds.
///
/// # Errors
///
/// When it does not hold within [`PROMPT`].
async fn until(what: &str, mut looking: impl FnMut() -> bool) -> Result<(), Failed> {
    let started = Instant::now();
    while started.elapsed() < PROMPT {
        if looking() {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
    Err(format!("{what} never happened within {PROMPT:?}").into())
}

/// Copies the built server into `home` and starts it.
///
/// # Errors
///
/// When the copy or the start fails, or the socket never answers.
async fn start(home: &mut Home) -> Result<(), Failed> {
    let binary = home.binary();
    std::fs::copy(SERVER, &binary)?;
    std::fs::copy(SERVER, home.path.join("iznik-server.previous"))?;
    let child = Command::new(&binary)
        .env("XDG_RUNTIME_DIR", &home.path)
        .arg("--foreground")
        .arg("--program")
        .arg("/bin/sh")
        .arg("--idle-shutdown-seconds")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    home.child = Some(child);
    let socket = home.socket();
    until("the daemon answers", || socket.exists()).await
}

/// A connected client that has made the one session.
///
/// # Errors
///
/// When the daemon cannot be greeted or will not make the session.
async fn connected(socket: &Path) -> Result<(TestClient<UnixStream>, DaemonInstance), Failed> {
    let mut client = TestClient::connect(socket).await?;
    client.auto_credit(true);
    let greeting = client.hello(Capabilities::INSTANCE).await?;
    let instance = greeting.instance.ok_or("the greeting named no instance")?;
    let made = client
        .command(SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: 80,
            rows: 24,
            working_directory: None,
        })
        .await?;
    if !matches!(made, CommandOutcome::Applied { .. }) {
        return Err(format!("the session was not made: {made:?}").into());
    }
    client.subscribe(PANE).await?;
    Ok((client, instance))
}

/// A client that resumes the pane the replacement kept.
///
/// # Errors
///
/// When the daemon cannot be greeted.
async fn resumed(socket: &Path) -> Result<(TestClient<UnixStream>, DaemonInstance), Failed> {
    let mut client = TestClient::connect(socket).await?;
    client.auto_credit(true);
    let greeting = client.hello(Capabilities::INSTANCE).await?;
    let instance = greeting.instance.ok_or("the greeting named no instance")?;
    client.subscribe(PANE).await?;
    Ok((client, instance))
}

/// Asks `pane` to print `marker` and waits until that text comes back.
///
/// # Errors
///
/// When the text never comes back.
async fn say_on(
    client: &mut TestClient<UnixStream>,
    pane: PaneId,
    marker: &str,
) -> Result<(), Failed> {
    client
        .input(pane, format!("printf '{marker}\\n'\n").into_bytes())
        .await?;
    let started = Instant::now();
    while started.elapsed() < PROMPT {
        if String::from_utf8_lossy(client.bytes_of(pane)).contains(marker) {
            return Ok(());
        }
        client.next(READ).await?;
    }
    Err(format!(
        "{marker} was not printed: {}",
        String::from_utf8_lossy(client.bytes_of(pane))
    )
    .into())
}

/// Asks the shell to print `marker` and waits until that text comes back.
///
/// # Errors
///
/// When the text never comes back.
async fn say(client: &mut TestClient<UnixStream>, marker: &str) -> Result<(), Failed> {
    client
        .input(PANE, format!("printf '{marker}\\n'\n").into_bytes())
        .await?;
    let started = Instant::now();
    while started.elapsed() < PROMPT {
        if String::from_utf8_lossy(client.bytes_of(PANE)).contains(marker) {
            return Ok(());
        }
        client.next(READ).await?;
    }
    Err(format!(
        "{marker} was not printed: {}",
        String::from_utf8_lossy(client.bytes_of(PANE))
    )
    .into())
}

/// Asks the daemon to exec `staged`.
///
/// # Errors
///
/// When the request command cannot be started.
async fn request(home: &Home, staged: &Path) -> std::io::Result<std::process::Output> {
    Command::new(home.binary())
        .env("XDG_RUNTIME_DIR", &home.path)
        .arg("--adopt-request")
        .arg(staged)
        .output()
        .await
}

/// # Panics
///
/// When the shell does not answer twice after the replacement, or the daemon's
/// instance changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replacement_keeps_the_same_shell() {
    let case = async {
        let mut home = Home::new("kept")?;
        start(&mut home).await?;
        let socket = home.socket();
        let binary = home.binary();
        let instance = {
            let (mut client, instance) = connected(&socket).await?;
            say(&mut client, "kept-before").await?;
            instance
        };
        let asked = request(&home, &binary).await?;
        if !asked.status.success() {
            return Err(format!(
                "the replacement was refused: {}",
                String::from_utf8_lossy(&asked.stderr)
            )
            .into());
        }
        until("the replaced daemon answers", || {
            std::os::unix::net::UnixStream::connect(&socket).is_ok()
        })
        .await?;
        let (mut client, after) = resumed(&socket).await?;
        assert_eq!(after, instance, "the same daemon is answering");
        say(&mut client, "kept-one").await?;
        say(&mut client, "kept-two").await?;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a binary that cannot be executed takes the daemon down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_exec_leaves_the_daemon_serving() {
    let case = async {
        let mut home = Home::new("exec")?;
        start(&mut home).await?;
        let socket = home.socket();
        {
            let (mut client, _instance) = connected(&socket).await?;
            say(&mut client, "still-before").await?;
        }
        let missing = home.path.join("not-a-server");
        std::fs::write(&missing, b"not a binary\n")?;
        let asked = request(&home, &missing).await?;
        assert!(
            !asked.status.success(),
            "a binary that cannot be executed is refused"
        );
        let (mut client, _instance) = resumed(&socket).await?;
        say(&mut client, "still-after").await?;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a refusal does not put the previous binary back, or the shell is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_adoption_restores_the_previous_binary() {
    let case = async {
        let mut home = Home::new("rollback")?;
        start(&mut home).await?;
        let socket = home.socket();
        let binary = home.binary();
        {
            let (mut client, _instance) = connected(&socket).await?;
            say(&mut client, "rolled-before").await?;
        }
        std::fs::create_dir_all(home.runtime())?;
        std::fs::write(home.runtime().join("adopt.refuse"), b"refuse\n")?;
        let asked = request(&home, &binary).await?;
        if !asked.status.success() {
            return Err(format!(
                "the rollback did not come back up: {}",
                String::from_utf8_lossy(&asked.stderr)
            )
            .into());
        }
        let marker = home.runtime().join("adopt.refuse");
        until("the refusal is consumed", || !marker.exists()).await?;
        let (mut client, _instance) = resumed(&socket).await?;
        say(&mut client, "rolled-after").await?;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a truncated adoption record takes the shell down, or the refusal is
/// not named.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_truncated_record_restores_the_previous_server() {
    let case = async {
        let mut home = Home::new("truncated")?;
        start(&mut home).await?;
        let socket = home.socket();
        let binary = home.binary();
        {
            let (mut client, _instance) = connected(&socket).await?;
            say(&mut client, "truncated-before").await?;
        }
        let stage = home.path.join("stage");
        std::fs::write(
            &stage,
            format!(
                "#!/bin/sh\nprintf 'x' > \"$2\"\nexec '{}' \"$@\"\n",
                binary.display()
            ),
        )?;
        std::fs::set_permissions(&stage, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        let asked = request(&home, &stage).await?;
        if !asked.status.success() {
            return Err(format!(
                "the replacement was refused before it could roll back: {}",
                String::from_utf8_lossy(&asked.stderr)
            )
            .into());
        }
        let mut stderr = home
            .child
            .as_mut()
            .and_then(|child| child.stderr.take())
            .ok_or("the daemon has no stderr")?;
        let mut heard = Vec::new();
        let mut buffer = [0; 1_024];
        let started = Instant::now();
        while started.elapsed() < PROMPT {
            if let Ok(Ok(count)) = tokio::time::timeout(POLL, stderr.read(&mut buffer)).await
                && count > 0
            {
                heard.extend(buffer.get(..count).unwrap_or_default());
            }
            if String::from_utf8_lossy(&heard).contains("truncated") {
                break;
            }
        }
        if !String::from_utf8_lossy(&heard).contains("truncated") {
            return Err(format!(
                "the refusal was not named: {}",
                String::from_utf8_lossy(&heard)
            )
            .into());
        }
        let record = home.runtime().join("adopted.state");
        until("the record is restored", || {
            std::fs::metadata(&record).is_ok_and(|meta| meta.len() > 1)
        })
        .await?;
        let (mut client, _instance) = resumed(&socket).await?;
        say(&mut client, "truncated-after").await?;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}

/// Rewrites the second carried pane id so adoption builds the first pane and
/// then refuses the second. The bytes stay a record the decoder accepts.
const SECOND_RECORD: &str = concat!(
    "import sys\n",
    "path = sys.argv[1]\n",
    "data = bytearray(open(path, \"rb\").read())\n",
    "pos = 18\n",
    "model_len = int.from_bytes(data[pos:pos + 4], \"little\")\n",
    "pos += 4 + model_len\n",
    "count = int.from_bytes(data[pos:pos + 4], \"little\")\n",
    "pos += 4\n",
    "def skip(pos):\n",
    "    length = int.from_bytes(data[pos:pos + 4], \"little\")\n",
    "    return pos + 4 + length\n",
    "pos += 24\n",
    "pos = skip(pos)\n",
    "pos = skip(pos)\n",
    "if count < 2:\n",
    "    raise SystemExit(\"expected two panes\")\n",
    "data[pos:pos + 8] = (99).to_bytes(8, \"little\")\n",
    "open(path, \"wb\").write(data)\n",
);

/// # Panics
///
/// When the first shell is dead after the second pane fails to build.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_second_pane_leaves_the_first_shell() {
    let case = async {
        let mut home = Home::new("second")?;
        start(&mut home).await?;
        let socket = home.socket();
        let binary = home.binary();
        {
            let (mut client, _instance) = connected(&socket).await?;
            say_on(&mut client, PANE, "first-before").await?;
            let made = client
                .command(SessionCommand::CreateSession {
                    name: "other".to_owned(),
                    columns: 80,
                    rows: 24,
                    working_directory: None,
                })
                .await?;
            if !matches!(made, CommandOutcome::Applied { .. }) {
                return Err(format!("the second session was not made: {made:?}").into());
            }
            client.subscribe(PaneId(2)).await?;
            say_on(&mut client, PaneId(2), "second-before").await?;
        }
        let stage = home.path.join("stage");
        std::fs::write(
            &stage,
            format!(
                "#!/bin/sh\npython3 -c '{SECOND_RECORD}' \"$2\"\nexec '{binary}' \"$@\"\n",
                SECOND_RECORD = SECOND_RECORD,
                binary = binary.display()
            ),
        )?;
        std::fs::set_permissions(&stage, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        let asked = request(&home, &stage).await?;
        if !asked.status.success() {
            return Err(format!(
                "the replacement was refused before it could roll back: {}",
                String::from_utf8_lossy(&asked.stderr)
            )
            .into());
        }
        let mut stderr = home
            .child
            .as_mut()
            .and_then(|child| child.stderr.take())
            .ok_or("the daemon has no stderr")?;
        let mut heard = Vec::new();
        let mut buffer = [0; 1_024];
        let started = Instant::now();
        while started.elapsed() < PROMPT {
            if let Ok(Ok(count)) = tokio::time::timeout(POLL, stderr.read(&mut buffer)).await
                && count > 0
            {
                heard.extend(buffer.get(..count).unwrap_or_default());
            }
            if String::from_utf8_lossy(&heard).contains("no carried record") {
                break;
            }
        }
        if !String::from_utf8_lossy(&heard).contains("no carried record") {
            return Err(format!(
                "the second pane was not the refusal: {}",
                String::from_utf8_lossy(&heard)
            )
            .into());
        }
        let (mut client, _instance) = resumed(&socket).await?;
        say_on(&mut client, PANE, "first-after").await?;
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
