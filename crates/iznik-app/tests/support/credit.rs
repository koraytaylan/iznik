//! Actual transport credit follows one surface's consumption independently of its sibling.

use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{AppContext, TestAppContext, WindowHandle};
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::grid::GridMetrics;
use iznik_app::host_ui::HostUi;
use iznik_app::surface::PaneSurface;
use iznik_app::vt::{
    PaneKey, TerminalSnapshot, TerminalTheme, VtEvent, VtOptions, VtOutput, VtThread,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::command::{Placement, SessionCommand};
use iznik_protocol::model::SplitDirection;

use super::container;

#[path = "../fixtures/pane_credit.rs"]
mod fixture;

/// Surface, container and transport failures propagate to the regression case.
type Failed = Box<dyn std::error::Error>;
/// Every process or frame observation fails within this budget.
const WAIT_DEADLINE: Duration = Duration::from_secs(10);
/// Yield to the real owner threads without spinning a core.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Bound owner drains just as the production window bounds its update cycle.
const MAXIMUM_EVENTS: usize = 256;

/// The normal application seams, with one consumer deliberately holding its owned replies.
struct Fixture {
    /// One engine and its authoritative model mirror.
    hosts: HostUi,
    /// Native owner shared by both pane surfaces.
    thread: Rc<VtThread>,
    /// Real GPUI pane views, keyed by their actual server identities.
    panes: BTreeMap<PaneKey, WindowHandle<PaneSurface>>,
    /// Only this surface may defer consuming its replies.
    paused: Option<PaneKey>,
    /// Replies retain their production delivery receipts until the surface consumes them.
    pending: VecDeque<VtEvent>,
    /// Saturating transport counters expose an unexpected extra grant without wrapping.
    received: BTreeMap<PaneKey, u64>,
    /// Host identity is the test-owned relay, never developer SSH configuration.
    host: HostId,
    /// Container lifetime extends beyond engine and native owners.
    _container: container::Container,
    /// Maximum time for an actual process response or credit boundary.
    deadline: Duration,
    /// Scheduling yield between ready owner drains.
    interval: Duration,
    /// Maximum ready messages read from either owner in one fixture update.
    maximum_events: usize,
}

impl Fixture {
    /// Attach one engine to the standard fixture without synthetic model or byte events.
    ///
    /// # Errors
    /// Returns fixture, engine or native owner startup failures.
    fn start(context: &mut TestAppContext) -> Result<Self, Failed> {
        context.update(gpui_kit::init);
        let container = container::Container::start(container::Options::default())?;
        let host = HostId(container.alias());
        let bridge = container.bridge("credit")?;
        bridge.add_host(&host.0)?;
        Ok(Self {
            hosts: HostUi::new(bridge),
            thread: Rc::new(VtThread::start(VtOptions::default())?),
            panes: BTreeMap::new(),
            paused: None,
            pending: VecDeque::new(),
            received: BTreeMap::new(),
            host,
            _container: container,
            deadline: WAIT_DEADLINE,
            interval: POLL_INTERVAL,
            maximum_events: MAXIMUM_EVENTS,
        })
    }

    /// Route production events; a held snapshot deliberately has no grid consumption or credit.
    ///
    /// # Errors
    /// Returns native submission or surface consumption failures.
    fn pump(&mut self, context: &mut TestAppContext) -> Result<(), Failed> {
        for _event in 0..self.maximum_events {
            let Some(event) = self.hosts.bridge().poll() else {
                break;
            };
            if let EngineEvent::Said(said) = &event {
                if let ManagerEvent::Bytes {
                    host, pane, bytes, ..
                } = said
                {
                    let key = PaneKey {
                        host: host.clone(),
                        pane: *pane,
                    };
                    let count = self.received.entry(key).or_default();
                    *count = count.saturating_add(u64::try_from(bytes.len())?);
                }
                EngineBridge::feed_terminal(&self.thread, said, &TerminalTheme::default())?;
            }
            self.hosts.absorb_event(event);
        }
        for _event in 0..self.maximum_events {
            let event = if self.paused.is_none() {
                self.pending.pop_front().or_else(|| self.thread.poll())
            } else {
                self.thread.poll()
            };
            let Some(event) = event else {
                break;
            };
            if self.paused.as_ref() == Some(&event.key) {
                self.pending.push_back(event);
            } else {
                let handle = self
                    .panes
                    .get(&event.key)
                    .ok_or("reply for absent surface")?;
                handle.update(context, |surface, _, context| {
                    surface.receive(event, self.hosts.bridge(), context)
                })??;
            }
        }
        for handle in self.panes.values() {
            context.update_window((*handle).into(), |_, window, application| {
                window.draw(application).clear(application);
            })?;
        }
        context.run_until_parked();
        Ok(())
    }

    /// Observe a real state transition while both owner channels remain serviced.
    ///
    /// # Errors
    /// Returns owner errors or a missing observation under the fixture deadline.
    fn wait<Value>(
        &mut self,
        context: &mut TestAppContext,
        mut observe: impl FnMut(&Self, &gpui_kit::App) -> Option<Value>,
    ) -> Result<Value, Failed> {
        let started = Instant::now();
        loop {
            self.pump(context)?;
            if let Some(value) = context.read(|application| observe(self, application)) {
                return Ok(value);
            }
            if started.elapsed() >= self.deadline {
                return Err(format!(
                    "surface credit deadline; received {:?}; pending {}",
                    self.received,
                    self.pending_bytes()
                )
                .into());
            }
            std::thread::sleep(self.interval);
        }
    }

    /// Make two real panes through commands, then subscribe their production surfaces.
    ///
    /// # Errors
    /// Returns command, model, subscription or window failures.
    fn create_pair(&mut self, context: &mut TestAppContext) -> Result<(PaneKey, PaneKey), Failed> {
        self.wait(context, |held, _| {
            held.hosts.state().model().host(&held.host).map(|_| ())
        })?;
        self.hosts.bridge().command(
            &self.host.0,
            SessionCommand::CreateSession {
                name: "surface-credit".to_owned(),
                columns: fixture::COLUMNS,
                rows: fixture::ROWS,
                working_directory: None,
            },
        )?;
        let (tab, first) = self.wait(context, |held, _| {
            let tab = held
                .hosts
                .state()
                .model()
                .host(&held.host)?
                .model
                .sessions
                .first()?
                .tabs
                .first()?;
            Some((tab.id, tab.panes.first()?.id))
        })?;
        self.hosts.bridge().command(
            &self.host.0,
            SessionCommand::CreatePane {
                tab,
                placement: Placement {
                    target: first,
                    direction: SplitDirection::Horizontal,
                    before: false,
                },
                columns: fixture::COLUMNS,
                rows: fixture::ROWS,
                working_directory: None,
            },
        )?;
        let second = self.wait(context, |held, _| {
            held.hosts
                .state()
                .model()
                .host(&held.host)?
                .model
                .sessions
                .first()?
                .tabs
                .first()?
                .panes
                .iter()
                .find(|pane| pane.id != first)
                .map(|pane| pane.id)
        })?;
        let first = PaneKey {
            host: self.host.clone(),
            pane: first,
        };
        let second = PaneKey {
            host: self.host.clone(),
            pane: second,
        };
        for key in [&first, &second] {
            let handle = context.add_window(|_, context| {
                PaneSurface::new(
                    key.clone(),
                    GridMetrics::default(),
                    Rc::clone(&self.thread),
                    context,
                )
            });
            self.panes.insert(key.clone(), handle);
            self.hosts.bridge().subscribe(&self.host.0, key.pane)?;
            self.wait(context, |held, application| {
                held.snapshot(application, key).map(|_| ())
            })?;
            let (prefix, tail) = fixture::READY.split_once('-').ok_or("reader marker")?;
            let command =
                format!("stty -echo -icanon; printf '%s-%s\\n' '{prefix}' '{tail}'; exec cat\n");
            self.hosts
                .bridge()
                .input(&self.host.0, key.pane, command.into_bytes())?;
            self.frame(context, key, fixture::READY)?;
        }
        Ok((first, second))
    }

    /// Current accepted grid frame, independent of native replies still being held.
    fn snapshot<'application>(
        &'application self,
        application: &'application gpui_kit::App,
        key: &PaneKey,
    ) -> Option<&'application TerminalSnapshot> {
        self.panes
            .get(key)?
            .read(application)
            .ok()?
            .grid()
            .read(application)
            .snapshot()
    }

    /// Wait for a marker that actually reached a consumed native frame.
    ///
    /// # Errors
    /// Returns owner or observation failures.
    fn frame(
        &mut self,
        context: &mut TestAppContext,
        key: &PaneKey,
        marker: &str,
    ) -> Result<TerminalSnapshot, Failed> {
        self.wait(context, |held, application| {
            let snapshot = held.snapshot(application, key)?;
            let text: String = snapshot
                .rows
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.text.as_str()))
                .collect();
            text.contains(marker).then(|| snapshot.clone())
        })
    }

    /// Delivery bytes waiting for this surface to consume their snapshots.
    fn pending_bytes(&self) -> u64 {
        self.pending
            .iter()
            .filter_map(|event| match &event.result {
                Ok(Some(VtOutput::Snapshot(snapshot))) => Some(u64::from(snapshot.consumed_bytes)),
                _ => None,
            })
            .fold(0, u64::saturating_add)
    }
}

#[gpui_kit::test]
#[ignore = "starts the two-container fixture and observes real per-pane transport credit"]
fn surface_credit_is_per_pane(context: &mut TestAppContext) {
    check(&consumption(context));
}

/// Holding native replies exhausts one actual channel while another remains interactive.
///
/// # Errors
/// Returns fixture, owner or transport failures.
///
/// # Panics
/// Fails on excess credit, cross-pane stalling, or incomplete recovery after consumption.
fn consumption(context: &mut TestAppContext) -> Result<(), Failed> {
    let mut held = Fixture::start(context)?;
    let (slow, healthy) = held.create_pair(context)?;
    let before = held.frame(context, &slow, fixture::READY)?;
    let received_before = held.received.get(&slow).copied().unwrap_or_default();
    held.paused = Some(slow.clone());
    let mut payload = vec![b'x'; fixture::FLOOD_BYTES];
    payload.extend_from_slice(fixture::FINISHED.as_bytes());
    let sent = u64::try_from(payload.len())?;
    held.hosts
        .bridge()
        .input(&held.host.0, slow.pane, payload)?;
    let capacity = u64::try_from(fixture::CREDIT_BYTES)?;
    held.wait(context, |held, _| {
        (held.pending_bytes() >= capacity).then_some(())
    })?;
    for marker in [fixture::FIRST, fixture::SECOND] {
        held.hosts
            .bridge()
            .input(&held.host.0, healthy.pane, marker.as_bytes().to_vec())?;
        held.frame(context, &healthy, marker)?;
        assert_eq!(
            held.pending_bytes(),
            capacity,
            "an unconsumed surface receives only its initial credit window"
        );
        assert_eq!(
            held.received
                .get(&slow)
                .copied()
                .unwrap_or_default()
                .saturating_sub(received_before),
            capacity,
            "healthy round trips cannot grant credit to the held pane"
        );
        let retained = context
            .read(|application| held.snapshot(application, &slow).cloned())
            .ok_or("missing held snapshot")?;
        assert_eq!(
            retained.sequence, before.sequence,
            "held native replies cannot advance the grid"
        );
    }
    held.paused = None;
    let after = held.frame(context, &slow, fixture::FINISHED)?;
    assert_eq!(
        after.sequence.0.saturating_sub(before.sequence.0),
        sent,
        "consuming queued receipts releases exactly the complete process output"
    );
    assert_eq!(
        held.pending_bytes(),
        0,
        "all held receipts have reached the consuming surface"
    );
    Ok(())
}

/// Keep the assertion outside the GPUI macro's generated function.
///
/// # Panics
/// Fails with the precise fixture or flow-control error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}
