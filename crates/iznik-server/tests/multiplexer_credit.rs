//! A pane's credit window across the moments a client starts counting again:
//! every `PaneChannel` starts a new stream on the client, which returns credit
//! only for the stream it is on, so the server must start the window again too
//! or the pane leaks credit until it stops.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iznik_protocol::identity::PaneId;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::multiplexer::Multiplexer;
use iznik_server::multiplexer::credit::{FOCUSED_CREDIT_BYTES, INITIAL_CREDIT_BYTES};
use iznik_server::pty::spawn::Program;
use iznik_server::resume::StartRequest;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use tokio::sync::RwLock;

mod sink;

use sink::Collected;

/// The width the pane is created at.
const COLUMNS: u16 = 80;
/// The height it is created at.
const ROWS: u16 = 24;
/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_secs(30);
/// How long a case waits between looks at the shell.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How many pumps a case allows before it calls the multiplexer a spin.
const PUMP_LIMIT: usize = 20_000;
/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// One quietened `sh` pane and a multiplexer that watches nothing yet.
struct Rig {
    /// The host.
    registry: Arc<RwLock<Registry>>,
    /// Its only pane.
    pane: PaneId,
    /// Where frames go.
    sink: Collected,
    /// The thing under test.
    multiplexer: Multiplexer<Collected>,
}

impl Rig {
    /// The rig.
    ///
    /// # Errors
    ///
    /// When the host or its pane will not start.
    async fn new() -> Result<Rig, Failed> {
        let mut held = Registry::new(
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
        let _session = held
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await?;
        let pane = held
            .snapshot()
            .sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .map(|pane| pane.id)
            .next()
            .ok_or("the host holds no pane")?;
        let deltas = held.deltas();
        let registry = Arc::new(RwLock::new(held));
        let sink = Collected::new();
        let rig = Rig {
            registry: Arc::clone(&registry),
            pane,
            sink: sink.clone(),
            multiplexer: Multiplexer::new(registry, sink, deltas),
        };
        rig.ask("stty -opost -echo 2>/dev/null").await?;
        Ok(rig)
    }

    /// Tells the shell to do something.
    ///
    /// # Errors
    ///
    /// When the pane is gone or will not take it.
    async fn ask(&self, script: &str) -> Result<(), Failed> {
        let host = self.registry.read().await;
        let held = host.pane(self.pane).ok_or("the host holds no pane")?;
        Ok(held.input(format!("{script}\n").into_bytes())?)
    }

    /// Has the shell print `bytes` more bytes, and waits for all of them.
    ///
    /// # Errors
    ///
    /// When the pane is gone or never prints them.
    async fn print(&self, bytes: u32) -> Result<(), Failed> {
        let before = self.newest().await;
        self.ask(&format!("head -c {bytes} /dev/zero | tr '\\0' x"))
            .await?;
        let wanted = before.saturating_add(u64::from(bytes));
        while self.newest().await < wanted {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Ok(())
    }

    /// The sequence just past the pane's newest byte.
    async fn newest(&self) -> u64 {
        let host = self.registry.read().await;
        host.pane(self.pane).map_or(0, |held| held.state().newest.0)
    }

    /// Pumps, returning no credit, until nothing more is sent, and says how
    /// many bytes went out on `channel` meanwhile.
    ///
    /// # Errors
    ///
    /// The multiplexer's refusals, and when it never settles.
    async fn drain(&mut self, channel: u8) -> Result<u64, Failed> {
        self.sink.reset_counts();
        for _turn in 0..PUMP_LIMIT {
            if !self.multiplexer.pump().await? {
                return Ok(self.sink.count(channel));
            }
        }
        Err("the multiplexer never ran out of things to send".into())
    }

    /// The channel the pane is carried on.
    ///
    /// # Errors
    ///
    /// When it is not carried.
    fn channel(&self) -> Result<u8, Failed> {
        let frames = self.sink.take();
        frames
            .iter()
            .filter(|frame| frame.channel == iznik_protocol::message::CHANNEL_CONTROL)
            .filter_map(|frame| iznik_protocol::message::decode_to_client(&frame.payload).ok())
            .find_map(|message| match message {
                iznik_protocol::message::ToClient::PaneChannel { channel, .. } => Some(channel),
                _other => None,
            })
            .ok_or_else(|| "no channel was announced".into())
    }
}

/// # Panics
///
/// When a window a client has spent is carried across a re-announcement,
/// after which the client returns nothing for it and the pane stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_announced_again_starts_with_its_whole_window() {
    let case = async {
        let mut rig = Rig::new().await?;
        rig.multiplexer
            .subscribe(StartRequest::Subscribe { pane: rig.pane })
            .await?;
        let channel = rig.channel()?;
        rig.sink.keep_control();
        rig.print(INITIAL_CREDIT_BYTES.saturating_mul(2)).await?;
        let first = rig.drain(channel).await?;
        assert_eq!(
            first,
            u64::from(INITIAL_CREDIT_BYTES),
            "the window is spent"
        );

        rig.multiplexer
            .subscribe(StartRequest::ScreenRequest { pane: rig.pane })
            .await?;
        rig.print(INITIAL_CREDIT_BYTES).await?;
        let again = rig.drain(channel).await?;
        assert!(
            again > 0,
            "the re-announced channel carries bytes on a fresh window"
        );
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the case finishes")
        .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When focusing the pane already focused does not give back its window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn focus_on_the_focused_pane_restores_its_window() {
    let case = async {
        let mut rig = Rig::new().await?;
        rig.multiplexer
            .subscribe(StartRequest::Subscribe { pane: rig.pane })
            .await?;
        let channel = rig.channel()?;
        rig.sink.keep_control();
        rig.multiplexer.focus(rig.pane).await?;
        rig.print(FOCUSED_CREDIT_BYTES.saturating_add(INITIAL_CREDIT_BYTES))
            .await?;
        let first = rig.drain(channel).await?;
        assert_eq!(
            first,
            u64::from(FOCUSED_CREDIT_BYTES),
            "the window is spent"
        );

        rig.multiplexer.focus(rig.pane).await?;
        let again = rig.drain(channel).await?;
        assert!(again > 0, "focusing it again gives its window back");
        Ok::<(), Failed>(())
    };
    tokio::time::timeout(DEADLINE, case)
        .await
        .expect("the case finishes")
        .unwrap_or_else(|error| panic!("{error}"));
}
