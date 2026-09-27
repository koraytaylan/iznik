//! A pasted file is written into the pane's directory, in pieces, and a name
//! that is not one component is refused without ending the connection.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{CommandOutcome, Created, SessionCommand};
use iznik_protocol::identity::PaneId;
use iznik_protocol::message::{ErrorCode, ToClient};
use iznik_server::connection::serve;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::client::{Received, TestClient};
use tokio::net::UnixStream;
use tokio::sync::RwLock;

/// The width every pane here is created at.
const COLUMNS: u16 = 80;
/// The height every pane here is created at.
const ROWS: u16 = 24;
/// How long a case waits for one answer.
const PROMPT: Duration = Duration::from_secs(2);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A pasted file lands in the pane's directory, assembled from two pieces.
///
/// # Errors
///
/// When the host, the paste, or the written file cannot be checked.
///
/// # Panics
///
/// When the finished file is not named or its bytes differ.
#[tokio::test]
async fn upload_writes_a_file_into_the_pane_directory() -> Result<(), Failed> {
    let directory = std::env::temp_dir().join(format!("iznik-upload-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut client = connect().await?;
    let pane = make_session(&mut client, &directory).await?;
    client
        .upload(pane, "notes.txt", 0, false, b"hello".to_vec())
        .await?;
    client
        .upload(pane, "notes.txt", 5, true, b" world".to_vec())
        .await?;
    let accepted = until_accepted(&mut client).await?;
    let ToClient::UploadAccepted(accepted) = accepted else {
        return Err("the answer was not an acceptance".into());
    };
    let Some(path) = accepted.path else {
        return Err("the finished file was not named".into());
    };
    let written = std::fs::read(&path)?;
    assert_eq!(written, b"hello world");
    assert!(path.ends_with("notes.txt"), "{path}");
    let _removed = std::fs::remove_dir_all(&directory);
    Ok(())
}

/// A name that leaves the directory is refused, and the connection stays up.
///
/// # Errors
///
/// When the host cannot be stood up or the refusal does not arrive.
///
/// # Panics
///
/// When the refusal is not an upload refusal, or the connection does not
/// answer a ping afterwards.
#[tokio::test]
async fn upload_refuses_a_name_that_is_not_one_file() -> Result<(), Failed> {
    let directory =
        std::env::temp_dir().join(format!("iznik-upload-refuse-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut client = connect().await?;
    let pane = make_session(&mut client, &directory).await?;
    client
        .upload(pane, "../outside", 0, true, b"no".to_vec())
        .await?;
    let refusal = until_error(&mut client).await?;
    let ToClient::Error { code, .. } = refusal else {
        return Err("the answer was not a refusal".into());
    };
    assert_eq!(code, ErrorCode::Upload);
    client.ping().await?;
    let pong = client.next(PROMPT).await?;
    assert!(matches!(pong, Received::Control(ToClient::Pong)));
    let _removed = std::fs::remove_dir_all(&directory);
    Ok(())
}

/// A relative path is created under the pane's directory, including an empty one.
///
/// # Errors
///
/// When the host, the paste, or the written tree cannot be checked.
///
/// # Panics
///
/// When a finished path is missing or the bytes differ.
#[tokio::test]
async fn upload_writes_a_nested_file() -> Result<(), Failed> {
    let directory =
        std::env::temp_dir().join(format!("iznik-upload-nested-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut client = connect().await?;
    let pane = make_session(&mut client, &directory).await?;
    client.upload(pane, "notes/", 0, true, Vec::new()).await?;
    let created = until_accepted(&mut client).await?;
    let ToClient::UploadAccepted(created) = created else {
        return Err("the directory was not accepted".into());
    };
    let Some(path) = created.path else {
        return Err("the directory was not named".into());
    };
    assert!(std::fs::metadata(&path)?.is_dir(), "{path}");
    client
        .upload(pane, "notes/nested.txt", 0, true, b"inside".to_vec())
        .await?;
    let accepted = until_accepted(&mut client).await?;
    let ToClient::UploadAccepted(accepted) = accepted else {
        return Err("the nested file was not accepted".into());
    };
    let Some(nested) = accepted.path else {
        return Err("the nested file was not named".into());
    };
    assert_eq!(std::fs::read(&nested)?, b"inside");
    let _removed = std::fs::remove_dir_all(&directory);
    Ok(())
}

/// A piece after the link drops continues the temporary file the host kept.
///
/// # Errors
///
/// When the host, the paste, or the resumed file cannot be checked.
///
/// # Panics
///
/// When the resumed file does not hold both pieces.
#[tokio::test]
async fn upload_resumes_a_file_after_the_link_drops() -> Result<(), Failed> {
    let directory =
        std::env::temp_dir().join(format!("iznik-upload-resume-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let partial = directory.join(".iznik-partial-notes.txt");
    {
        let mut client = connect().await?;
        let pane = make_session(&mut client, &directory).await?;
        client
            .upload(pane, "notes.txt", 0, false, b"hello".to_vec())
            .await?;
        for _look in 0..40 {
            if std::fs::read(&partial).ok().as_deref() == Some(b"hello".as_slice()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    assert_eq!(std::fs::read(&partial)?, b"hello");
    let mut client = connect().await?;
    let pane = make_session(&mut client, &directory).await?;
    client
        .upload(pane, "notes.txt", 9, true, b"!".to_vec())
        .await?;
    let early = until_error(&mut client).await?;
    let ToClient::Error { code, message } = early else {
        return Err("a resume past the file was not refused".into());
    };
    assert_eq!(code, ErrorCode::Upload);
    assert!(message.contains("is at byte 5"), "{message}");
    client
        .upload(pane, "notes.txt", 5, true, b" world".to_vec())
        .await?;
    let accepted = until_accepted(&mut client).await?;
    let ToClient::UploadAccepted(accepted) = accepted else {
        return Err("the resumed file was not accepted".into());
    };
    let Some(path) = accepted.path else {
        return Err("the resumed file was not named".into());
    };
    assert_eq!(std::fs::read(path)?, b"hello world");
    let _removed = std::fs::remove_dir_all(&directory);
    Ok(())
}

/// A client that has shaken hands.
///
/// # Errors
///
/// When the mirror, the socket pair, or the handshake fails.
async fn connect() -> Result<TestClient<UnixStream>, Failed> {
    let registry = Registry::new(
        RegistryDefaults {
            program: Program::Command {
                path: "sh".into(),
                arguments: Vec::new(),
            },
            terminfo_directory: None,
            agent_socket: None,
            program_interval: Duration::ZERO,
        },
        Arc::new(Mutex::new(HistoryBudget::new(DEFAULT_HISTORY_BUDGET_BYTES))),
        MirrorThread::start()?,
    );
    let registry = Arc::new(RwLock::new(registry));
    let (near, far) = UnixStream::pair()?;
    tokio::spawn(serve(far, registry));
    let mut client = TestClient::over(near);
    let _greeting = client.hello(Capabilities::known()).await?;
    Ok(client)
}

/// Creates a session whose pane starts in `directory`.
///
/// # Errors
///
/// When the session is refused or it has no pane.
async fn make_session(
    client: &mut TestClient<UnixStream>,
    directory: &Path,
) -> Result<PaneId, Failed> {
    let outcome = client
        .command(SessionCommand::CreateSession {
            name: "work".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: Some(directory.display().to_string()),
        })
        .await?;
    let CommandOutcome::Applied {
        created: Created::Session(_session),
        ..
    } = outcome
    else {
        return Err(format!("the session was not created: {outcome:?}").into());
    };
    let model = client.snapshot().await?;
    let pane = model
        .sessions
        .first()
        .and_then(|session| session.tabs.first())
        .and_then(|tab| tab.panes.first())
        .map(|pane| pane.id)
        .ok_or("the session has no pane")?;
    Ok(pane)
}

/// The next acceptance, skipping the shell's own output.
///
/// # Errors
///
/// When the server refuses the file or never accepts it.
async fn until_accepted(client: &mut TestClient<UnixStream>) -> Result<ToClient, Failed> {
    for _look in 0..40 {
        match client.next(PROMPT).await? {
            Received::Control(message @ ToClient::UploadAccepted(_)) => return Ok(message),
            Received::Control(ToClient::Error { message, .. }) => {
                return Err(message.into());
            }
            Received::Control(_) | Received::PaneBytes { .. } => {}
        }
    }
    Err("the file was never accepted".into())
}

/// The next upload refusal, skipping the shell's own output.
///
/// # Errors
///
/// When the server never refuses the name.
async fn until_error(client: &mut TestClient<UnixStream>) -> Result<ToClient, Failed> {
    for _look in 0..40 {
        match client.next(PROMPT).await? {
            Received::Control(message @ ToClient::Error { .. }) => return Ok(message),
            Received::Control(_) | Received::PaneBytes { .. } => {}
        }
    }
    Err("the name was not refused".into())
}
