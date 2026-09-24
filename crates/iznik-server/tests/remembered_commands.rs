//! A command sent again after a dropped link is answered, not applied twice:
//! the host keeps what it answered each identified client, across that
//! client's connections, for so many commands and so long.
//!
//! Over socket pairs, like the other connection cases: no daemon, no socket
//! file. The panes run `sh`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iznik_link::framed::FramedLink;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::{
    CommandOutcome, SessionCommand, decode_command_outcome, encode_session_command,
};
use iznik_protocol::identity::{ClientIdentity, CommandId};
use iznik_protocol::message::{
    CHANNEL_CONTROL, PROTOCOL_VERSION, ToClient, ToServer, decode_to_client, encode_to_server,
};
use iznik_server::connection::serve;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::pty::spawn::Program;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::session::remembered::{
    REMEMBERED_CLIENTS, REMEMBERED_COMMANDS, REMEMBERED_FOR, RememberedCommands,
};
use iznik_server::terminal::mirror::MirrorThread;
use tokio::net::UnixStream;
use tokio::sync::RwLock;

/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A host with nothing in it, its panes running `sh`.
///
/// # Errors
///
/// When the mirror thread will not start.
fn host() -> Result<Arc<RwLock<Registry>>, Failed> {
    Ok(Arc::new(RwLock::new(Registry::new(
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
    ))))
}

/// A connection that has shaken hands and, when given one, said who it is.
///
/// # Errors
///
/// When the pair cannot be made or the handshake fails.
async fn connected(
    registry: &Arc<RwLock<Registry>>,
    client: Option<ClientIdentity>,
) -> Result<FramedLink<UnixStream>, Failed> {
    let (near, far) = UnixStream::pair()?;
    let _serving = tokio::spawn(serve(far, Arc::clone(registry)));
    let mut link = FramedLink::new(near);
    let greeting = ToServer::Hello {
        protocol_version: PROTOCOL_VERSION,
        client_version: "remembering".to_owned(),
        capabilities: Capabilities::from_bits(0),
    };
    link.send(CHANNEL_CONTROL, &encode_to_server(&greeting)?)
        .await?;
    let frame = link.next_frame().await?.ok_or("no greeting")?;
    let ToClient::Hello { capabilities, .. } = decode_to_client(frame.payload)? else {
        return Err("the first answer was not a Hello".into());
    };
    if !capabilities.contains(Capabilities::IDENTIFY) {
        return Err("the server does not say it remembers".into());
    }
    if let Some(client) = client {
        link.send(
            CHANNEL_CONTROL,
            &encode_to_server(&ToServer::Identify { client })?,
        )
        .await?;
    }
    Ok(link)
}

/// Sends a command under `number` and gives back its answer's bytes.
///
/// # Errors
///
/// When the link fails or no answer comes.
async fn asked(
    link: &mut FramedLink<UnixStream>,
    number: u64,
    command: &SessionCommand,
) -> Result<Vec<u8>, Failed> {
    let request = ToServer::Command {
        command_id: CommandId(number),
        payload: encode_session_command(command)?,
    };
    link.send(CHANNEL_CONTROL, &encode_to_server(&request)?)
        .await?;
    loop {
        let frame = link.next_frame().await?.ok_or("the connection closed")?;
        if let ToClient::CommandResult {
            command_id,
            payload,
        } = decode_to_client(frame.payload)?
            && command_id == CommandId(number)
        {
            return Ok(payload);
        }
    }
}

/// A session named `name`.
fn creating(name: &str) -> SessionCommand {
    SessionCommand::CreateSession {
        name: name.to_owned(),
        columns: 80,
        rows: 24,
        working_directory: None,
    }
}

/// # Panics
///
/// When a command an identified client sends again on its next connection,
/// under the same number, is applied a second time rather than answered with
/// what it was answered the first time — or when another client, or one that
/// never said who it is, is answered from somebody else's memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_sent_again_is_answered_once() {
    let case = async {
        let registry = host()?;
        let client = ClientIdentity(0x1234_5678_9abc_def0);
        let command = creating("work");

        let mut first = connected(&registry, Some(client)).await?;
        let answered = asked(&mut first, 7, &command).await?;
        assert!(
            matches!(
                decode_command_outcome(&answered)?,
                CommandOutcome::Applied { .. }
            ),
            "the first time, it is applied"
        );
        drop(first);

        let mut again = connected(&registry, Some(client)).await?;
        let repeated = asked(&mut again, 7, &command).await?;
        assert_eq!(repeated, answered, "the second time, the same answer");
        let sessions = registry.read().await.snapshot().sessions.len();
        assert_eq!(sessions, 1, "and nothing applied twice");

        let mut other = connected(&registry, Some(ClientIdentity(1))).await?;
        let _theirs = asked(&mut other, 7, &command).await?;
        let mut nameless = connected(&registry, None).await?;
        let _unnamed = asked(&mut nameless, 7, &command).await?;
        let all = registry.read().await.snapshot().sessions.len();
        assert_eq!(all, 3, "a number is one client's own");
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the case finishes")
        .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the memory keeps more than its bounds: more commands for one client,
/// more clients, or an answer past its age.
#[test]
fn the_memory_is_bounded() {
    let mut memory = RememberedCommands::default();
    let started = Instant::now();
    let client = ClientIdentity(1);
    let most = u64::try_from(REMEMBERED_COMMANDS).expect("a count");
    for number in 0..=most {
        memory.remember(client, CommandId(number), vec![1], started);
    }
    assert_eq!(
        memory.recall(client, CommandId(0), started),
        None,
        "the oldest command is forgotten"
    );
    assert_eq!(
        memory.recall(client, CommandId(most), started),
        Some(vec![1]),
        "the newest is kept"
    );

    let clients = u128::try_from(REMEMBERED_CLIENTS).expect("a count");
    for (turn, other) in (2..clients.saturating_add(2)).enumerate() {
        let later = started
            .checked_add(Duration::from_millis(u64::try_from(turn).expect("a turn")))
            .expect("a moment");
        memory.remember(ClientIdentity(other), CommandId(0), vec![2], later);
    }
    assert_eq!(
        memory.recall(client, CommandId(most), started),
        None,
        "the quietest client is forgotten"
    );

    let aged = started
        .checked_add(REMEMBERED_FOR)
        .and_then(|moment| moment.checked_add(Duration::from_secs(1)))
        .expect("a moment");
    assert_eq!(
        memory.recall(ClientIdentity(2), CommandId(0), aged),
        None,
        "and an answer past its age"
    );
}
