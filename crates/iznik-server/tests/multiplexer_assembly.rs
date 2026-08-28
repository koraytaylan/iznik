//! The pump proven against what a person would feel: a keystroke echoes while
//! another pane floods, background panes get equal shares, a pane that has
//! fallen far behind is sent the truth rather than carried byte by byte, and a
//! stalled cursor costs nothing. The acceptance is a number, so
//! `keystroke_latency_under_flood` measures a thousand round trips and reports
//! the tail beside the budget. Every pane runs `sh`, quietened with
//! `stty -opost -echo` first, so a case reads back exactly what it asked for.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iznik_protocol::command::Placement;
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::{CHANNEL_CONTROL, ToClient, decode_to_client};
use iznik_protocol::model::SplitDirection;
use iznik_server::history::{DEFAULT_HISTORY_BUDGET_BYTES, HistoryBudget};
use iznik_server::multiplexer::channel::SinkError;
use iznik_server::multiplexer::credit::{
    FOCUSED_CREDIT_BYTES, FRAME_PAYLOAD_LENGTH, INITIAL_CREDIT_BYTES, STALE_THRESHOLD_BYTES,
};
use iznik_server::multiplexer::{FrameSink, KEYSTROKE_ROUND_TRIP_BUDGET, Multiplexer};
use iznik_server::pty::spawn::Program;
use iznik_server::resume::StartRequest;
use iznik_server::session::registry::{Registry, RegistryDefaults};
use iznik_server::terminal::mirror::MirrorThread;
use iznik_testkit::metrics;
use iznik_testkit::vt::Vt;
use tokio::sync::RwLock;

/// The width every pane in these cases is created at.
const COLUMNS: u16 = 80;
/// The height they are created at.
const ROWS: u16 = 24;
/// The deadline every case runs under, so a stall is a named failure.
const DEADLINE: Duration = Duration::from_mins(2);
/// How long a case waits between looks at a shell.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How many looks it takes before it gives up on one.
const POLL_ATTEMPTS: usize = 4000;
/// How many unchanged looks finish a pane; one is a pause, not an end.
const QUIET_LOOKS: usize = 20;
/// How many pumps a case allows before it calls the multiplexer a spin.
const PUMP_LIMIT: usize = 20_000;
/// The unit the floods in these cases are stated in.
const MEBIBYTE: u64 = 1024 * 1024;
/// Anything a case can fail on, so a helper reports rather than panics.
type Failed = Box<dyn std::error::Error>;

/// A frame the multiplexer sent.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    /// The channel it went out on.
    channel: u8,
    /// Its bytes.
    payload: Vec<u8>,
}

/// What the sink has been handed.
#[derive(Debug)]
struct Sent {
    /// The frames it kept.
    frames: Vec<Frame>,
    /// How many bytes went out on each channel, kept or not.
    counts: BTreeMap<u8, u64>,
    /// Whether pane frames are kept: a flooding case turns this off.
    keeping: bool,
}

/// A sink that keeps frames in memory; plan 0004 puts a framed link here.
#[derive(Clone, Debug)]
struct Collected {
    /// What it has been handed.
    inner: Arc<Mutex<Sent>>,
}

impl Collected {
    /// A sink that keeps everything.
    fn new() -> Collected {
        Collected {
            inner: Arc::new(Mutex::new(Sent {
                frames: Vec::new(),
                counts: BTreeMap::new(),
                keeping: true,
            })),
        }
    }

    /// What it has been handed, locked.
    fn held(&self) -> std::sync::MutexGuard<'_, Sent> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps only control frames from here on, forgetting what it has.
    fn keep_control(&self) {
        let mut held = self.held();
        held.keeping = false;
        held.frames.clear();
        held.counts.clear();
    }

    /// The kept frames, and forgets them: what one step sent, not every step.
    fn take(&self) -> Vec<Frame> {
        std::mem::take(&mut self.held().frames)
    }

    /// How many bytes have gone out on a channel.
    fn count(&self, channel: u8) -> u64 {
        self.held().counts.get(&channel).copied().unwrap_or(0)
    }

    /// Forgets every count, so a case can measure one interval.
    fn reset_counts(&self) {
        self.held().counts.clear();
    }
}

impl FrameSink for Collected {
    fn send(
        &mut self,
        channel: u8,
        payload: &[u8],
    ) -> impl Future<Output = Result<(), SinkError>> + Send {
        let inner = Arc::clone(&self.inner);
        let frame = Frame {
            channel,
            payload: payload.to_vec(),
        };
        async move {
            let mut held = inner.lock().unwrap_or_else(PoisonError::into_inner);
            let carried = u64::try_from(frame.payload.len()).unwrap_or(0);
            let counted = held.counts.entry(frame.channel).or_insert(0);
            *counted = counted.saturating_add(carried);
            if held.keeping || frame.channel == CHANNEL_CONTROL {
                held.frames.push(frame);
            }
            Ok(())
        }
    }
}

/// The control messages among some frames, in order.
fn control(frames: &[Frame]) -> Vec<ToClient> {
    frames
        .iter()
        .filter(|frame| frame.channel == CHANNEL_CONTROL)
        .filter_map(|frame| decode_to_client(&frame.payload).ok())
        .collect()
}

/// The names of some frames' control messages, without their fields.
fn named(frames: &[Frame]) -> Vec<String> {
    control(frames)
        .iter()
        .map(|message| {
            format!("{message:?}")
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// Every byte sent on one channel, in order.
fn bytes_on(frames: &[Frame], channel: u8) -> Vec<u8> {
    frames
        .iter()
        .filter(|frame| frame.channel == channel)
        .flat_map(|frame| frame.payload.iter().copied())
        .collect()
}

/// The channel and sequence the first `PaneChannel` among some frames named.
fn announced(frames: &[Frame]) -> Option<(u8, Sequence)> {
    control(frames)
        .into_iter()
        .find_map(|message| match message {
            ToClient::PaneChannel {
                channel, sequence, ..
            } => Some((channel, sequence)),
            _other => None,
        })
}

/// The first `Screen` among some frames: where it is exact, and its bytes.
fn screen_in(frames: &[Frame]) -> Option<(Sequence, Vec<u8>)> {
    control(frames)
        .into_iter()
        .find_map(|message| match message {
            ToClient::Screen {
                sequence, bytes, ..
            } => Some((sequence, bytes)),
            _other => None,
        })
}

/// What a terminal shows after being fed some bytes.
///
/// # Errors
///
/// When the oracle cannot be created or read.
fn shown(pieces: &[&[u8]]) -> Result<String, Failed> {
    let mut oracle = Vt::new(COLUMNS, ROWS)?;
    for piece in pieces {
        oracle.feed(piece);
    }
    Ok(oracle.snapshot()?)
}

/// The value `upper` parts in `lower` of the way through a sorted set.
fn percentile(sorted: &[Duration], upper: usize, lower: usize) -> Duration {
    let last = sorted.len().saturating_sub(1);
    sorted
        .len()
        .saturating_mul(upper)
        .checked_div(lower)
        .and_then(|at| sorted.get(at.min(last)))
        .copied()
        .unwrap_or_default()
}

/// A shell command that floods a pane with about `mebibytes` mebibytes of
/// printable lines, each as wide as the pane.
fn flood(mebibytes: u64) -> String {
    let line: String = std::iter::repeat_n('x', COLUMNS.into()).collect();
    let width = u64::from(COLUMNS).saturating_add(1);
    let wanted = MEBIBYTE.saturating_mul(mebibytes).saturating_add(width);
    let lines = wanted.checked_div(width).unwrap_or_default();
    format!("yes {line} | head -n {lines}")
}

/// Runs a case under the deadline.
///
/// # Errors
///
/// Whatever the case reports, and when it does not finish in [`DEADLINE`].
async fn bounded<Case: Future<Output = Result<(), Failed>>>(case: Case) -> Result<(), Failed> {
    tokio::time::timeout(DEADLINE, case).await?
}

/// A host, a sink, and the multiplexer between them.
struct Rig {
    /// The host model and the panes behind it.
    registry: Arc<RwLock<Registry>>,
    /// The panes it was made with, in model order.
    panes: Vec<PaneId>,
    /// Where the multiplexer's frames go.
    sink: Collected,
    /// The thing under test.
    multiplexer: Multiplexer<Collected>,
}

impl Rig {
    /// A host of `count` quietened `sh` panes whose history comes out of a
    /// budget of `budget` bytes, with a multiplexer watching none of them.
    ///
    /// # Errors
    ///
    /// When the mirror thread, the registry or one of its panes will not
    /// start.
    async fn new(budget: usize, count: usize) -> Result<Rig, Failed> {
        let mut held = Registry::new(
            RegistryDefaults {
                program: Program::Command {
                    path: "sh".into(),
                    arguments: Vec::new(),
                },
                terminfo_directory: None,
            },
            Arc::new(Mutex::new(HistoryBudget::new(budget))),
            MirrorThread::start()?,
        );
        let session = held
            .create_session("work".to_owned(), COLUMNS, ROWS, None)
            .await?;
        let tab = held
            .snapshot()
            .sessions
            .iter()
            .find(|made| made.id == session)
            .and_then(|made| made.tabs.first().map(|tab| tab.id))
            .ok_or("the new session held no tab")?;
        for _more in 1..count {
            let target = *panes_of(&held).last().ok_or("the tab held no pane")?;
            let placement = Placement {
                target,
                direction: SplitDirection::Horizontal,
                before: false,
            };
            let _made = held
                .create_pane(tab, placement, COLUMNS, ROWS, None)
                .await?;
        }
        let panes = panes_of(&held);
        let deltas = held.deltas();
        let registry = Arc::new(RwLock::new(held));
        let sink = Collected::new();
        let rig = Rig {
            registry: Arc::clone(&registry),
            panes: panes.clone(),
            sink: sink.clone(),
            multiplexer: Multiplexer::new(registry, sink, deltas),
        };
        for pane in &panes {
            rig.ask(*pane, "stty -opost -echo 2>/dev/null").await?;
        }
        Ok(rig)
    }

    /// One of the host's panes.
    ///
    /// # Errors
    ///
    /// When the host was not made with that many.
    fn pane(&self, at: usize) -> Result<PaneId, Failed> {
        self.panes
            .get(at)
            .copied()
            .ok_or_else(|| format!("the host holds no pane {at}").into())
    }

    /// Renames the host's only tab, the cheapest delta there is.
    ///
    /// # Errors
    ///
    /// When the host holds no tab, or refuses the name.
    async fn rename(&self, turn: usize) -> Result<(), Failed> {
        let mut held = self.registry.write().await;
        let tab = held
            .snapshot()
            .sessions
            .first()
            .and_then(|session| session.tabs.first().map(|tab| tab.id))
            .ok_or("the host holds no tab")?;
        Ok(held.rename_tab(tab, format!("tab {turn}"))?)
    }

    /// Tells a pane's shell to do something.
    ///
    /// # Errors
    ///
    /// When the host holds no such pane, or its input cannot be accepted.
    async fn ask(&self, pane: PaneId, script: &str) -> Result<(), Failed> {
        let host = self.registry.read().await;
        let held = host.pane(pane).ok_or("the host holds no such pane")?;
        Ok(held.input(format!("{script}\n").into_bytes())?)
    }

    /// The sequence just past a pane's newest byte.
    async fn newest(&self, pane: PaneId) -> Sequence {
        let host = self.registry.read().await;
        host.pane(pane)
            .map_or(Sequence(0), |held| held.state().newest)
    }

    /// Everything a pane has produced from `from`.
    ///
    /// # Errors
    ///
    /// When the host holds no such pane, or the ring has aged past `from`.
    async fn history(&self, pane: PaneId, from: Sequence) -> Result<Vec<u8>, Failed> {
        let host = self.registry.read().await;
        let held = host.pane(pane).ok_or("the host holds no such pane")?;
        Ok(held.read_history(from)?)
    }

    /// Waits until a pane has produced at least `wanted` bytes.
    ///
    /// # Errors
    ///
    /// When it has not after [`POLL_ATTEMPTS`] looks.
    async fn produced(&self, pane: PaneId, wanted: u64) -> Result<(), Failed> {
        for _attempt in 0..POLL_ATTEMPTS {
            if self.newest(pane).await.0 >= wanted {
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Err(format!("pane {} never produced {wanted} bytes", pane.0).into())
    }

    /// Waits until a pane has been unchanged for [`QUIET_LOOKS`] consecutive
    /// looks. Not one: a flooding shell on a loaded machine pauses for longer
    /// than a look, and a case that took the pause for the end would compare a
    /// prefix against a whole.
    async fn quiescent(&self, pane: PaneId) {
        let (mut before, mut still) = (Sequence(0), 0_usize);
        for _attempt in 0..POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
            let now = self.newest(pane).await;
            still = usize::from(now == before && now.0 > 0).saturating_mul(still.saturating_add(1));
            if still >= QUIET_LOOKS {
                return;
            }
            before = now;
        }
    }

    /// Fills every pane with [`STOCKED_BYTES`] and waits for all of it, so a
    /// scheduling case measures the scheduler and not the shells.
    ///
    /// # Errors
    ///
    /// When a pane will not take its instruction or never produces enough.
    async fn stock(&self) -> Result<(), Failed> {
        let pouring = flood(STOCKED_BYTES / MEBIBYTE);
        for pane in &self.panes {
            self.ask(*pane, &pouring).await?;
        }
        for pane in &self.panes {
            self.produced(*pane, STOCKED_BYTES).await?;
        }
        Ok(())
    }

    /// Holds a delivery to the pane's own record: the bytes are the history
    /// from `at`, and the screen before them reproduces the pane's mirror.
    ///
    /// # Errors
    ///
    /// When the pane is gone, the oracle refuses, or either does not hold.
    async fn exact(
        &self,
        pane: PaneId,
        at: Sequence,
        screen: &[u8],
        after: &[u8],
    ) -> Result<(), Failed> {
        if after != self.history(pane, at).await? {
            return Err("the bytes are not the history from the screen".into());
        }
        let held = self.registry.read().await.pane(pane).cloned();
        let mirror = held.ok_or("the host holds no such pane")?.screen().await?;
        if shown(&[screen, after])? != shown(&[&mirror.bytes])? {
            return Err("screen then bytes do not reproduce the mirror".into());
        }
        Ok(())
    }

    /// Begins a subscription and says which channel it was given, alongside
    /// everything the multiplexer sent starting it.
    ///
    /// # Errors
    ///
    /// The multiplexer's refusals, and when it announced no channel.
    async fn attach(&mut self, request: StartRequest) -> Result<(u8, Vec<Frame>), Failed> {
        self.multiplexer.subscribe(request).await?;
        let frames = self.sink.take();
        let (channel, _at) = announced(&frames).ok_or("no channel was announced")?;
        Ok((channel, frames))
    }

    /// Begins watching a pane from where it is now, and says only which
    /// channel it was given.
    ///
    /// # Errors
    ///
    /// As [`Rig::attach`].
    async fn watch(&mut self, pane: PaneId) -> Result<u8, Failed> {
        let request = StartRequest::Subscribe { pane };
        let (channel, _started) = self.attach(request).await?;
        Ok(channel)
    }

    /// Pumps until nothing more is ready, topping `crediting` up by a frame's
    /// worth each turn, or returning no credit at all when it is `None`.
    ///
    /// # Errors
    ///
    /// The multiplexer's refusals, and when it never settles.
    async fn drain(&mut self, crediting: Option<u8>) -> Result<(), Failed> {
        for _turn in 0..PUMP_LIMIT {
            if let Some(channel) = crediting {
                self.multiplexer.credit(channel, FRAME_PAYLOAD_LENGTH)?;
            }
            if !self.multiplexer.pump().await? {
                return Ok(());
            }
        }
        Err("the multiplexer never ran out of things to send".into())
    }
}

/// Every pane a host holds, in model order.
fn panes_of(registry: &Registry) -> Vec<PaneId> {
    registry
        .snapshot()
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .map(|pane| pane.id)
        .collect()
}

/// How much each pane produces before a scheduling case starts: more than any
/// round can carry, and short of the lag that would mark a cursor stale.
const STOCKED_BYTES: u64 = 3 * MEBIBYTE;
/// How many rounds a scheduling case runs for.
const ROUNDS: usize = 16;
/// How many changes a client misses, being more than the channel holds.
const LAGGING_RENAMES: usize = 2000;
/// How many keystroke round trips the latency case measures.
const SAMPLES: usize = 1000;
/// The percentile the budget is stated at, over [`OF`].
const AT: usize = 99;
/// The middle of the distribution, reported beside the tail.
const MIDDLE: usize = 50;
/// What [`AT`] and [`MIDDLE`] are percentiles of.
const OF: usize = 100;
/// How long one keystroke may take before the case calls the pump stopped.
const SAMPLE_DEADLINE: Duration = Duration::from_secs(5);
/// How much the multiplexer's own memory may grow across a flood: one frame's
/// buffer and the allocator's slack, but nothing proportional to the flood.
const MEMORY_SLACK_BYTES: u64 = 16 * MEBIBYTE;
/// What a stalled cursor may cost over an idle interval, which is the
/// resolution of the clock `/proc` reports rather than a budget.
const IDLE_CPU_CEILING: Duration = Duration::from_millis(20);
/// How long that idle interval is.
const IDLE_INTERVAL: Duration = Duration::from_millis(200);
/// What a subscription that needs the truth opens with.
fn cold_path() -> Vec<String> {
    vec!["PaneChannel".to_owned(), "Screen".to_owned()]
}

/// # Panics
///
/// When the model's changes do not reach the client on channel 0 in the order
/// the registry made them, when a client forced past the delta channel's
/// capacity is not sent the whole model instead of what it can no longer be
/// told, or when a mark reaches it for a pane it does not watch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deltas_and_marks_fan_out_in_order() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 2).await?;
        let (watched, unwatched) = (rig.pane(0)?, rig.pane(1)?);
        let _channel = rig.watch(watched).await?;
        for turn in 0..ROUNDS {
            rig.rename(turn).await?;
        }
        for pane in [watched, unwatched] {
            rig.ask(pane, r"printf '\033]2;titled\007'").await?;
            rig.quiescent(pane).await;
        }
        rig.drain(None).await?;

        let frames = rig.sink.take();
        let messages = control(&frames);
        let generations: Vec<u64> = messages
            .iter()
            .filter_map(|message| match message {
                ToClient::Delta { generation, .. } => Some(generation.0),
                _other => None,
            })
            .collect();
        assert_eq!(generations.len(), ROUNDS, "one delta per rename");
        let stepping = generations
            .windows(2)
            .all(|pair| pair.last().copied() == pair.first().map(|held| held.saturating_add(1)));
        assert!(stepping, "the generation advances by one: {generations:?}");
        let kinds = named(&frames);
        let owed = kinds.contains(&"Snapshot".to_owned());
        assert!(!owed, "a client that missed nothing: {kinds:?}");

        let marks: Vec<(PaneId, Sequence)> = messages
            .iter()
            .filter_map(|message| match message {
                ToClient::Mark { pane, sequence, .. } => Some((*pane, *sequence)),
                _other => None,
            })
            .collect();
        assert!(!marks.is_empty(), "the watched pane's title was a mark");
        let only = marks.iter().all(|(pane, at)| *pane == watched && at.0 > 0);
        assert!(only, "only watched marks, each saying where: {marks:?}");

        // Past DELTA_BROADCAST_CAPACITY without pumping, so the receiver is
        // told it has missed changes rather than handed them.
        for turn in 0..LAGGING_RENAMES {
            rig.rename(turn).await?;
        }
        rig.drain(None).await?;
        let missed = named(&rig.sink.take());
        let snapshots = missed.iter().filter(|kind| *kind == "Snapshot").count();
        assert_eq!(snapshots, 1, "exactly one snapshot: {missed:?}");
        let last = missed.last().map(String::as_str);
        assert_eq!(
            last,
            Some("Snapshot"),
            "and it is the last word: {missed:?}"
        );
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a subscribed pane that exits does not detach, or its channel number is
/// handed out again before the client says nothing of it is still in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pane_that_exits_detaches_and_holds_its_channel() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 2).await?;
        let (going, staying) = (rig.pane(0)?, rig.pane(1)?);
        let taken = rig.watch(going).await?;

        rig.ask(going, "exit").await?;
        for _attempt in 0..POLL_ATTEMPTS {
            rig.registry.write().await.ingest();
            if rig.registry.read().await.pane(going).is_none() {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        rig.drain(None).await?;

        let frames = rig.sink.take();
        let detached = control(&frames).iter().any(|message| {
            matches!(message, ToClient::PaneDetached { pane, channel }
                if *pane == going && *channel == taken)
        });
        assert!(detached, "it detached: {:?}", named(&frames));
        let watching = rig.multiplexer.subscribed();
        assert!(watching.is_empty(), "and nothing is watched: {watching:?}");

        let next = rig.watch(staying).await?;
        assert_ne!(next, taken, "a channel in flight is not handed out again");
        rig.multiplexer.unsubscribe(staying).await?;
        let _detached = rig.sink.take();
        rig.multiplexer.channel_released(taken)?;
        let again = rig.watch(staying).await?;
        assert_eq!(again, taken, "an acknowledged channel is free again");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a cold attach does not begin with the truth, when the megabyte that
/// follows it does not begin exactly where that truth ends, or when a screen
/// request leaves the cursor where a byte is delivered twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_is_exact_under_load() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 1).await?;
        let pane = rig.pane(0)?;
        let pouring = flood(1);
        rig.ask(pane, &pouring).await?;
        rig.produced(pane, MEBIBYTE / 4).await?;

        let (channel, started) = rig.attach(StartRequest::Subscribe { pane }).await?;
        let opening = named(&started);
        assert_eq!(opening, cold_path(), "the channel is announced first");
        let (at, screen) = screen_in(&started).ok_or("no screen was sent")?;
        let place = announced(&started).map(|(_channel, place)| place);
        assert_eq!(place, Some(at), "announced where the screen is exact");

        rig.quiescent(pane).await;
        rig.drain(Some(channel)).await?;
        let after = bytes_on(&rig.sink.take(), channel);
        rig.exact(pane, at, &screen, &after).await?;

        let (again, asked) = rig.attach(StartRequest::ScreenRequest { pane }).await?;
        let answer = named(&asked);
        assert_eq!(answer, cold_path(), "a screen request answers with truth");
        assert_eq!(again, channel, "on the channel the pane already has");
        let (moved, truth) = screen_in(&asked).ok_or("no screen was sent")?;
        let now = rig.newest(pane).await;
        assert_eq!(moved, now, "the screen is exact at all the pane has said");

        rig.ask(pane, &pouring).await?;
        rig.produced(pane, moved.0.saturating_add(MEBIBYTE)).await?;
        rig.quiescent(pane).await;
        rig.drain(Some(channel)).await?;
        let onward = bytes_on(&rig.sink.take(), channel);
        rig.exact(pane, moved, &truth, &onward).await?;
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a hot reconnect costs a screen it does not need, when the bytes before
/// and after the break do not join into what the pane said, or when a resume
/// from before the ring's oldest byte is answered with bytes that are gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_continues_or_takes_the_cold_path() {
    bounded(async {
        // Small enough that a flood later ages the ring past where the client
        // is, which is the only difference between the two answers.
        let capacity = 64 * 1024;
        let mut rig = Rig::new(capacity, 1).await?;
        let pane = rig.pane(0)?;
        let (channel, started) = rig.attach(StartRequest::Subscribe { pane }).await?;
        let (at, screen) = screen_in(&started).ok_or("no screen was sent")?;

        rig.ask(pane, r"printf 'while attached\r\n'").await?;
        rig.quiescent(pane).await;
        rig.drain(None).await?;
        let before = bytes_on(&rig.sink.take(), channel);
        let parted = Sequence(at.0.saturating_add(u64::try_from(before.len())?));

        rig.multiplexer.unsubscribe(pane).await?;
        let _detached = rig.sink.take();
        rig.ask(pane, r"printf 'while away\r\n'").await?;
        rig.quiescent(pane).await;

        let (again, resumed) = rig
            .attach(StartRequest::Resume { pane, from: parted })
            .await?;
        let kinds = named(&resumed);
        assert_eq!(kinds, vec!["PaneChannel".to_owned()], "no screen was owed");
        let place = announced(&resumed).map(|(_channel, place)| place);
        assert_eq!(place, Some(parted), "it resumes where it left off");
        rig.drain(None).await?;
        let mut joined = before;
        joined.extend_from_slice(&bytes_on(&rig.sink.take(), again));
        rig.exact(pane, at, &screen, &joined).await?;

        rig.multiplexer.unsubscribe(pane).await?;
        let _parting = rig.sink.take();
        rig.ask(pane, &flood(1)).await?;
        rig.produced(pane, u64::try_from(4 * capacity)?).await?;
        rig.quiescent(pane).await;

        let request = StartRequest::Resume { pane, from: at };
        let (carrying, reopened) = rig.attach(request).await?;
        assert_eq!(named(&reopened), cold_path(), "it takes the cold path");
        let (aged, truth) = screen_in(&reopened).ok_or("no screen was sent")?;
        assert!(
            aged > at,
            "the truth is where the pane is, not where it was"
        );
        rig.ask(pane, r"printf 'after the catch up\r\n'").await?;
        rig.quiescent(pane).await;
        rig.drain(Some(carrying)).await?;
        let onward = bytes_on(&rig.sink.take(), carrying);
        rig.exact(pane, aged, &truth, &onward).await?;
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the ninety-ninth percentile of a thousand keystroke-to-echo round
/// trips, taken while another pane floods, is not under
/// [`KEYSTROKE_ROUND_TRIP_BUDGET`].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keystroke_latency_under_flood() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 2).await?;
        let (typed, flooding) = (rig.pane(0)?, rig.pane(1)?);
        let endless = format!("while :; do {}; done", flood(1));
        rig.ask(flooding, &endless).await?;
        // `cat` on a pseudoterminal in canonical mode echoes a line when one
        // arrives, which is the smallest honest stand-in for a keystroke.
        rig.ask(typed, "cat").await?;
        rig.produced(flooding, MEBIBYTE).await?;

        let echoing = rig.watch(typed).await?;
        let flooded = rig.watch(flooding).await?;
        rig.multiplexer.focus(typed).await?;
        rig.sink.keep_control();

        let mut trips = Vec::with_capacity(SAMPLES);
        for turn in 0..SAMPLES {
            rig.multiplexer.credit(echoing, FRAME_PAYLOAD_LENGTH)?;
            rig.multiplexer.credit(flooded, FRAME_PAYLOAD_LENGTH)?;
            let before = rig.sink.count(echoing);
            let started = Instant::now();
            rig.ask(typed, &format!("{turn}")).await?;
            while rig.sink.count(echoing) == before {
                if !rig.multiplexer.pump().await? {
                    tokio::task::yield_now().await;
                }
                if started.elapsed() > SAMPLE_DEADLINE {
                    return Err(format!("keystroke {turn} was never echoed").into());
                }
            }
            trips.push(started.elapsed());
        }
        let poured = rig.sink.count(flooded);
        assert!(
            poured >= MEBIBYTE,
            "the other pane really flooded: {poured}"
        );

        trips.sort_unstable();
        let tail = percentile(&trips, AT, OF);
        let middle = percentile(&trips, MIDDLE, OF);
        let distribution = format!(
            "p{MIDDLE} {middle:?}, worst {:?}, over {} samples against {} flood bytes",
            trips.last(),
            trips.len(),
            rig.sink.count(flooded)
        );
        let within = tail < KEYSTROKE_ROUND_TRIP_BUDGET;
        assert!(within, "p{AT} {tail:?} is over budget ({distribution})");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When background panes with equal amounts to say do not receive equal shares
/// of the link, or when focus does not move first service and the larger
/// window to the pane the person is looking at.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scheduler_shares_the_link_and_follows_focus() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 3).await?;
        let mut channels = Vec::new();
        for at in 0..rig.panes.len() {
            let pane = rig.pane(at)?;
            channels.push(rig.watch(pane).await?);
        }
        // Subscribed before it is stocked: a cursor starts at the pane's
        // newest byte, so a pane filled first has nothing behind its cursor.
        rig.sink.keep_control();
        rig.stock().await?;
        for _round in 0..ROUNDS {
            for channel in &channels {
                rig.multiplexer.credit(*channel, FRAME_PAYLOAD_LENGTH)?;
            }
            let _sent = rig.multiplexer.pump().await?;
        }
        let each: Vec<u64> = channels.iter().map(|held| rig.sink.count(*held)).collect();
        let most = each.iter().max().copied().unwrap_or_default();
        let least = each.iter().min().copied().unwrap_or_default();
        assert!(least > 0, "every background pane was served: {each:?}");
        let spread = most.saturating_sub(least) <= u64::from(FRAME_PAYLOAD_LENGTH);
        assert!(spread, "none is more than a frame ahead: {each:?}");

        // No credit is returned from here on, so what each cursor delivers is
        // exactly the window focus gave it.
        let watched = *channels.first().ok_or("no first channel")?;
        let background = *channels.get(1).ok_or("no second channel")?;
        rig.multiplexer.focus(rig.pane(0)?).await?;
        rig.sink.reset_counts();
        rig.drain(None).await?;
        let ceiling = u64::from(FOCUSED_CREDIT_BYTES);
        let increment = ceiling.saturating_sub(INITIAL_CREDIT_BYTES.into());
        let gained = rig
            .sink
            .count(watched)
            .saturating_sub(rig.sink.count(background));
        assert_eq!(gained, increment, "the focused pane's window is larger");

        // Focusing what is already focused would otherwise add the increment
        // again and send bytes the client never granted credit for.
        rig.sink.reset_counts();
        rig.multiplexer.focus(rig.pane(0)?).await?;
        rig.drain(None).await?;
        let twice = rig.sink.count(watched);
        assert_eq!(twice, 0, "focusing twice does not widen twice: {twice}");

        rig.sink.reset_counts();
        rig.multiplexer.focus(rig.pane(1)?).await?;
        let _sent = rig.multiplexer.pump().await?;
        let served = rig.sink.count(background) > 0 && rig.sink.count(watched) == 0;
        assert!(served, "focus moves first service with it");
        rig.drain(None).await?;
        let moved = rig.sink.count(background) > rig.sink.count(watched);
        assert!(moved, "and the larger window moves with it too");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a cursor whose client stops returning credit keeps receiving frames,
/// stops the others, or spins; or when the multiplexer's own memory grows with
/// what it was not asked to carry — a stalled cursor's bytes or an unwatched
/// pane's flood.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credit_is_honored_and_nothing_is_buffered() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 3).await?;
        let (silent, returning, unwatched) = (rig.pane(0)?, rig.pane(1)?, rig.pane(2)?);
        let stalled = rig.watch(silent).await?;
        let flowing = rig.watch(returning).await?;
        rig.sink.keep_control();
        rig.stock().await?;

        rig.ask(unwatched, &flood(u64::try_from(ROUNDS)?)).await?;
        let before = metrics::resident_memory(std::process::id())?;
        rig.drain(Some(flowing)).await?;
        let carried = u64::try_from(ROUNDS)?.saturating_mul(MEBIBYTE);
        rig.produced(unwatched, STOCKED_BYTES.saturating_add(carried))
            .await?;

        let after = metrics::resident_memory(std::process::id())?;

        let held = rig.sink.count(stalled);
        let window = u64::from(INITIAL_CREDIT_BYTES);
        assert_eq!(held, window, "a silent client's cursor stops at its window");
        let flowed = rig.sink.count(flowing) > rig.sink.count(stalled);
        assert!(flowed, "while every other cursor continues");
        let watching = rig.multiplexer.subscribed();
        assert!(!watching.contains(&unwatched), "the flood was not watched");
        let grew = after.saturating_sub(before);
        assert!(grew < MEMORY_SLACK_BYTES, "held {grew} bytes of the flood");

        // Nothing is left with credit, so a pump that spins rather than waits
        // shows up as processor time over an interval where nothing happens.
        rig.quiescent(unwatched).await;
        let spent_before = metrics::cpu_time(std::process::id())?;
        let idling = async {
            loop {
                let _sent = rig.multiplexer.pump().await?;
                rig.multiplexer.ready().await;
            }
        };
        let stopped: Result<(), Failed> = tokio::time::timeout(IDLE_INTERVAL, idling)
            .await
            .unwrap_or(Ok(()));
        stopped?;
        let spent = metrics::cpu_time(std::process::id())?.saturating_sub(spent_before);
        assert!(spent < IDLE_CPU_CEILING, "{spent:?} idle is a spin");

        // And the credit returning is what must wake it: the pane it is
        // waiting on may be at a prompt, saying nothing for hours.
        rig.multiplexer.credit(stalled, FRAME_PAYLOAD_LENGTH)?;
        let woke = tokio::time::timeout(IDLE_INTERVAL, rig.multiplexer.ready()).await;
        assert!(woke.is_ok(), "returning credit wakes a parked pump");
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a background pane that has fallen further behind than
/// [`STALE_THRESHOLD_BYTES`] keeps being carried byte by byte, or is not sent
/// the truth when it is looked at.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_cursor_is_caught_up_on_focus() {
    bounded(async {
        let mut rig = Rig::new(DEFAULT_HISTORY_BUDGET_BYTES, 2).await?;
        let (falling, held) = (rig.pane(0)?, rig.pane(1)?);
        let behind = rig.watch(falling).await?;
        let _channel = rig.watch(held).await?;
        rig.multiplexer.focus(held).await?;

        // Its window is spent while it is still close enough to be carried, so
        // that when it does fall behind there is no credit to catch it up with
        // — which is what a client that has stopped reading looks like.
        rig.ask(falling, &flood(1)).await?;
        rig.produced(falling, MEBIBYTE).await?;
        rig.quiescent(falling).await;
        rig.drain(None).await?;
        let spent = rig.sink.count(behind);
        assert_eq!(
            spent,
            u64::from(INITIAL_CREDIT_BYTES),
            "its window is spent"
        );

        let lagged = STALE_THRESHOLD_BYTES.saturating_add(2 * MEBIBYTE);
        rig.ask(falling, &flood(lagged / MEBIBYTE)).await?;
        rig.produced(falling, lagged).await?;
        rig.quiescent(falling).await;
        rig.sink.reset_counts();
        rig.drain(None).await?;
        let carried = rig.sink.count(behind);
        assert_eq!(carried, 0, "a cursor too far behind is not carried");
        let quiet = rig.sink.take();
        assert!(screen_in(&quiet).is_none(), "and is not caught up either");

        rig.multiplexer.focus(falling).await?;
        rig.drain(Some(behind)).await?;
        let frames = rig.sink.take();
        let (at, screen) = screen_in(&frames).ok_or("no screen was sent")?;
        let opened = announced(&frames);
        assert_eq!(opened, Some((behind, at)), "announced where truth is exact");
        rig.ask(falling, r"printf 'after the catch up\r\n'").await?;
        rig.quiescent(falling).await;
        rig.drain(Some(behind)).await?;
        let mut after = bytes_on(&frames, behind);
        after.extend_from_slice(&bytes_on(&rig.sink.take(), behind));
        rig.exact(falling, at, &screen, &after).await?;
        Ok::<(), Failed>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
}
