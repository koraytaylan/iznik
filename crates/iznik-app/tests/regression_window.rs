//! Production window routing over SSH inside the repository container fixture.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle};
use iznik_app::vt::{PaneKey, TerminalSnapshot, VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;
use iznik_protocol::command::{Placement, SessionCommand};
use iznik_protocol::model::{HostModel, LayoutNode, SplitDirection, Weighted};
use libghostty_vt::terminal::ScrollViewport;

#[path = "fixtures/window_cold.rs"]
mod cold;
#[path = "support/container.rs"]
mod container;
#[path = "fixtures/window_lifecycle.rs"]
mod fixture;

/// Window, transport and fixture failures are reported by the owning test.
type Failed = Box<dyn std::error::Error>;
/// Every observation is bounded independently of nextest's outer regression deadline.
const WAIT_DEADLINE: Duration = Duration::from_secs(10);
/// Yield to real engine and VT threads between deterministic headless draws.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Test-owned runtime and window, with no synthetic engine or terminal events.
struct WindowFixture {
    /// Owns the real daemon, SSH credentials and isolated relay.
    container: container::Container,
    /// Actual production shell rendered by the headless test platform.
    window: WindowHandle<WindowShell>,
    /// Alias used consistently in engine and terminal identities.
    host: HostId,
    /// Additional real windows sharing this container host with independent engines.
    windows: Vec<WindowHandle<WindowShell>>,
    /// Maximum time to observe one operation's result.
    deadline: Duration,
    /// Brief scheduling yield between UI updates.
    interval: Duration,
}

impl WindowFixture {
    /// Start the real container stack and attach one application window to it.
    ///
    /// # Errors
    /// Returns fixture, engine, terminal or window startup errors.
    fn start(context: &mut TestAppContext) -> Result<Self, Failed> {
        context.update(gpui_kit::init);
        let container = container::Container::start(container::Options::default())?;
        let host = HostId(container.alias());
        let bridge = container.bridge("first")?;
        bridge.add_host(&host.0)?;
        let window = open_window(context, bridge)?;
        Ok(Self {
            container,
            window,
            host,
            windows: Vec::new(),
            deadline: WAIT_DEADLINE,
            interval: POLL_INTERVAL,
        })
    }

    /// Pump production routing and draw until an actual UI observation satisfies the predicate.
    ///
    /// # Errors
    /// Returns a closed window or a bounded observation failure with relay diagnostics.
    fn wait<Value>(
        &self,
        context: &mut TestAppContext,
        mut observe: impl FnMut(&WindowShell, &gpui_kit::App) -> Option<Value>,
    ) -> Result<Value, Failed> {
        let started = Instant::now();
        loop {
            let value = self.window.update(context, |shell, window, context| {
                shell.update(window, context);
                observe(shell, context)
            })?;
            context.update_window(self.window.into(), |_, window, application| {
                window.draw(application).clear(application);
            })?;
            for handle in &self.windows {
                handle.update(context, |shell, window, context| {
                    shell.update(window, context);
                })?;
                context.update_window((*handle).into(), |_, window, application| {
                    window.draw(application).clear(application);
                })?;
            }
            context.run_until_parked();
            if let Some(value) = value {
                return Ok(value);
            }
            if started.elapsed() >= self.deadline {
                let reports = self.window.update(context, |shell, _, _| {
                    format!("{:?}", shell.hosts().state().hosts().collect::<Vec<_>>())
                })?;
                return Err(format!(
                    "window observation deadline; {reports}; relay {:?}",
                    self.container.failures()
                )
                .into());
            }
            std::thread::sleep(self.interval);
        }
    }

    /// Attach another production window with its own engine and native terminal owner.
    ///
    /// # Errors
    /// Returns engine or terminal startup failures.
    fn another_window(
        &mut self,
        context: &mut TestAppContext,
    ) -> Result<WindowHandle<WindowShell>, Failed> {
        let bridge = self.container.bridge("second")?;
        bridge.add_host(&self.host.0)?;
        let window = open_window(context, bridge)?;
        self.windows.push(window);
        Ok(window)
    }

    /// Submit a command through the same bridge used by application actions.
    ///
    /// # Errors
    /// Returns a closed window or engine submission failure.
    fn command(&self, context: &mut TestAppContext, command: SessionCommand) -> Result<(), Failed> {
        self.window.update(context, |shell, _, _| {
            shell.hosts().bridge().command(&self.host.0, command)
        })??;
        Ok(())
    }

    /// Create the first pane and wait for its actual server screen.
    ///
    /// # Errors
    /// Returns command, model, subscription or window failures.
    fn create(&self, context: &mut TestAppContext) -> Result<PaneKey, Failed> {
        self.wait(context, |shell, _| self.model(shell).map(|_| ()))?;
        self.command(
            context,
            SessionCommand::CreateSession {
                name: fixture::SESSION.to_owned(),
                columns: fixture::COLUMNS,
                rows: fixture::ROWS,
                working_directory: None,
            },
        )?;
        let key = self.wait(context, |shell, _| {
            let session = self
                .model(shell)?
                .sessions
                .iter()
                .find(|session| session.name == fixture::SESSION)?;
            let pane = session.tabs.first()?.panes.first()?.id;
            Some(PaneKey {
                host: self.host.clone(),
                pane,
            })
        })?;
        self.wait(context, |shell, application| {
            shell
                .surface(&key)?
                .read(application)
                .grid()
                .read(application)
                .snapshot()
                .map(|_| ())
        })?;
        Ok(key)
    }

    /// Cut or restore the transport and observe the production connection state and banner.
    ///
    /// # Errors
    /// Returns a missing connection transition or missing visible failure banner.
    fn connect(&self, context: &mut TestAppContext, connected: bool) -> Result<(), Failed> {
        self.container.set_connected(connected);
        self.wait(context, |shell, _| {
            let report = shell.hosts().state().host(&self.host)?;
            (matches!(report.connection, HostState::Connected { .. }) == connected).then_some(())
        })?;
        if !connected {
            let identifier = format!("host-banner-{}", self.host.0);
            let visible = context.update_window(self.window.into(), |_, window, _| {
                window
                    .find(gpui_kit::SharedString::from(identifier))
                    .visible()
            })?;
            if !visible {
                return Err("missing transport failure banner".into());
            }
        }
        Ok(())
    }

    /// Produce enough real terminal output to make a client-owned history viewport.
    ///
    /// # Errors
    /// Returns input submission or window errors.
    fn history(&self, context: &mut TestAppContext, key: &PaneKey) -> Result<(), Failed> {
        self.window.update(context, |shell, _, _| {
            shell.hosts().bridge().input(
                &self.host.0,
                key.pane,
                b"printf 'history row\\n%.0s' $(seq 1 120)\n".to_vec(),
            )
        })??;
        Ok(())
    }

    /// The current authoritative model after production engine events have been applied.
    fn model<'shell>(&self, shell: &'shell WindowShell) -> Option<&'shell HostModel> {
        shell
            .hosts()
            .state()
            .model()
            .host(&self.host)
            .map(|host| &host.model)
    }

    /// Send a shell marker whose literal command cannot itself contain the complete marker.
    ///
    /// # Errors
    /// Returns a closed window or input submission failure.
    fn print(
        &self,
        context: &mut TestAppContext,
        key: &PaneKey,
        marker: &str,
    ) -> Result<(), Failed> {
        let (prefix, tail) = marker.split_once('-').ok_or("marker delimiter")?;
        let input = format!("printf '%s-%s\\n' '{prefix}' '{tail}'\n").into_bytes();
        self.window.update(context, |shell, _, _| {
            shell.hosts().bridge().input(&self.host.0, key.pane, input)
        })??;
        Ok(())
    }

    /// Change the viewport through the grid's retained production scroll subscription.
    ///
    /// # Errors
    /// Returns a closed window or missing pane.
    fn scroll(
        &self,
        context: &mut TestAppContext,
        key: &PaneKey,
        scroll: ScrollViewport,
    ) -> Result<(), Failed> {
        self.window.update(context, |shell, _, context| {
            let surface = shell.surface(key).ok_or("missing pane")?;
            let grid = surface.read(context).grid().clone();
            grid.update(context, |grid, context| grid.scroll(scroll, context));
            Ok::<_, Failed>(())
        })??;
        Ok(())
    }

    /// Wait for real process output to appear in the rendered pane's native snapshot.
    ///
    /// # Errors
    /// Returns window or observation failures.
    fn frame(
        &self,
        context: &mut TestAppContext,
        key: &PaneKey,
        marker: &str,
    ) -> Result<TerminalSnapshot, Failed> {
        self.wait(context, |shell, application| {
            let snapshot = shell
                .surface(key)?
                .read(application)
                .grid()
                .read(application)
                .snapshot()?;
            let text: String = snapshot
                .rows
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.text.as_str()))
                .collect();
            text.contains(marker).then(|| snapshot.clone())
        })
    }
}

#[gpui_kit::test]
#[ignore = "starts the two-container fixture and stages static binaries"]
fn window_container_create_and_hot_resume(context: &mut TestAppContext) {
    check(&lifecycle(context));
}

/// Exercise actual command, model, subscription, output and reconnect routes.
///
/// # Errors
/// Returns fixture, window or production engine failures.
///
/// # Panics
/// Fails if reconnect replaces the pane entity or loses ordered terminal output.
fn lifecycle(context: &mut TestAppContext) -> Result<(), Failed> {
    let held = WindowFixture::start(context)?;
    let key = held.create(context)?;
    held.history(context, &key)?;
    held.print(context, &key, fixture::BEFORE)?;
    let before = held.frame(context, &key, fixture::BEFORE)?;
    let identity = held.window.update(context, |shell, _, _| {
        shell.surface(&key).map(Entity::entity_id)
    })?;
    held.scroll(context, &key, ScrollViewport::Top)?;
    held.wait(context, |shell, application| {
        let snapshot = shell
            .surface(&key)?
            .read(application)
            .grid()
            .read(application)
            .snapshot()?;
        (!snapshot.viewport.at_bottom()).then_some(())
    })?;
    held.connect(context, false)?;
    held.connect(context, true)?;
    held.print(context, &key, fixture::AFTER)?;
    let after = held.wait(context, |shell, application| {
        let snapshot = shell
            .surface(&key)?
            .read(application)
            .grid()
            .read(application)
            .snapshot()?;
        (snapshot.sequence > before.sequence).then(|| snapshot.clone())
    })?;
    assert!(
        !after.viewport.at_bottom(),
        "hot resume retains the client-owned history viewport; a screen reset would return to bottom"
    );
    held.scroll(context, &key, ScrollViewport::Bottom)?;
    held.frame(context, &key, fixture::AFTER)?;
    assert!(
        after.sequence > before.sequence,
        "new process output advances the held terminal"
    );
    assert_eq!(
        held.window.update(context, |shell, _, _| shell
            .surface(&key)
            .map(Entity::entity_id))?,
        identity,
        "reconnect retains the same pane entity"
    );
    held.frame(context, &key, fixture::BEFORE)?;
    Ok(())
}

/// Keep assertions outside the GPUI macro's generated function.
///
/// # Panics
/// Fails with the bounded fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

#[gpui_kit::test]
#[ignore = "starts the two-container fixture and fills the remote history ring"]
fn window_container_cold_resume_and_next_output(context: &mut TestAppContext) {
    check(&cold_resume(context));
}

/// An expired resume cursor must replace the retained emulator before new output arrives.
///
/// # Errors
/// Returns fixture, script, transport or window failures.
///
/// # Panics
/// Fails if a cold screen preserves the old viewport or loses its sequence baseline.
fn cold_resume(context: &mut TestAppContext) -> Result<(), Failed> {
    let held = WindowFixture::start(context)?;
    let key = held.create(context)?;
    held.history(context, &key)?;
    let script = cold_script(held.interval)?;
    held.window.update(context, |shell, _, _| {
        shell
            .hosts()
            .bridge()
            .input(&held.host.0, key.pane, script.into_bytes())
    })??;
    let before = held.frame(context, &key, cold::READY)?;
    held.scroll(context, &key, ScrollViewport::Top)?;
    held.wait(context, |shell, application| {
        let snapshot = shell
            .surface(&key)?
            .read(application)
            .grid()
            .read(application)
            .snapshot()?;
        (!snapshot.viewport.at_bottom()).then_some(())
    })?;
    held.connect(context, false)?;
    held.container.run_host(
        &format!(
            "touch {}; while [ ! -f {} ]; do sleep {}; done",
            cold::RELEASE,
            cold::COMPLETE,
            held.interval.as_secs_f64()
        ),
        held.deadline,
    )?;
    held.connect(context, true)?;
    let reset = held.frame(context, &key, cold::FINISHED)?;
    assert!(
        reset.viewport.at_bottom(),
        "cold screen resets the old client viewport"
    );
    assert!(
        reset.sequence.0.saturating_sub(before.sequence.0) >= cold::FLOOD_BYTES,
        "resume crossed more than the server's entire history ring"
    );
    held.print(context, &key, fixture::AFTER)?;
    let after = held.frame(context, &key, fixture::AFTER)?;
    assert!(
        after.sequence > reset.sequence,
        "after output starts after the authoritative cold screen"
    );
    held.frame(context, &key, cold::FINISHED)?;
    Ok(())
}

/// A file-controlled script ensures the ring expires while the window is disconnected.
/// Marker parts remain separate in the echoed command so only executed output can satisfy them.
///
/// # Errors
/// Returns an invalid marker fixture.
fn cold_script(interval: Duration) -> Result<String, Failed> {
    let (ready_prefix, ready_tail) = cold::READY.split_once('-').ok_or("ready marker")?;
    let (finished_prefix, finished_tail) =
        cold::FINISHED.split_once('-').ok_or("finished marker")?;
    Ok(format!(
        "printf '%s-%s\\n' '{ready_prefix}' '{ready_tail}'; while [ ! -f {} ]; do sleep {}; done; head -c {} /dev/zero | tr '\\000' x; printf '\\n%s-%s\\n' '{finished_prefix}' '{finished_tail}'; touch {}\n",
        cold::RELEASE,
        interval.as_secs_f64(),
        cold::FLOOD_BYTES,
        cold::COMPLETE
    ))
}

/// Initial window width makes two equal panes narrower than their creation dimensions.
const INITIAL_WIDTH: f32 = 800.0;
/// The second client owns a visibly wider surface.
const SECOND_WIDTH: f32 = 1_200.0;
/// A later resize on the first client must supersede the second client's geometry.
const FINAL_WIDTH: f32 = 1_600.0;
/// Height is fixed while width changes, isolating horizontal geometry ownership.
const WINDOW_HEIGHT: f32 = 400.0;
/// A revised layout gives the original pane three quarters of the horizontal split.
const LARGER_WEIGHT: u32 = 3;

#[gpui_kit::test]
#[ignore = "starts the two-container fixture and two independent application windows"]
fn window_container_split_deltas_and_last_resize(context: &mut TestAppContext) {
    check(&split_and_resize(context));
}

/// Real model commands change layout while another client competes for geometry.
///
/// # Errors
/// Returns command, fixture or window failures.
///
/// # Panics
/// Fails when model changes rebuild the original surface or clients fight over geometry.
fn split_and_resize(context: &mut TestAppContext) -> Result<(), Failed> {
    let mut held = WindowFixture::start(context)?;
    resize_window(context, held.window, INITIAL_WIDTH);
    let key = held.create(context)?;
    let (tab, identity) = held.window.update(context, |shell, _, _| {
        (
            shell.selected().map(|selected| selected.tab),
            shell.surface(&key).map(Entity::entity_id),
        )
    })?;
    let tab = tab.ok_or("missing selected tab")?;
    held.command(
        context,
        SessionCommand::CreatePane {
            tab,
            placement: Placement {
                target: key.pane,
                direction: SplitDirection::Horizontal,
                before: false,
            },
            columns: fixture::COLUMNS,
            rows: fixture::ROWS,
            working_directory: None,
        },
    )?;
    let other_pane = held.wait(context, |shell, _| {
        held.model(shell)?
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|candidate| candidate.id == tab)?
            .panes
            .iter()
            .find(|pane| pane.id != key.pane)
            .map(|pane| pane.id)
    })?;
    let narrow = held.wait(context, |shell, application| {
        let columns = dimensions(shell, application, &key)?;
        (columns < fixture::COLUMNS).then_some(columns)
    })?;
    held.command(
        context,
        SessionCommand::SetLayout {
            tab,
            layout: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                children: vec![
                    Weighted {
                        node: LayoutNode::Leaf(key.pane),
                        weight: LARGER_WEIGHT,
                    },
                    Weighted {
                        node: LayoutNode::Leaf(other_pane),
                        weight: 1,
                    },
                ],
            },
        },
    )?;
    let wider = held.wait(context, |shell, application| {
        let columns = dimensions(shell, application, &key)?;
        (columns > narrow).then_some(columns)
    })?;
    assert_eq!(
        held.window.update(context, |shell, _, _| shell
            .surface(&key)
            .map(Entity::entity_id))?,
        identity,
        "real split deltas retain the existing pane entity"
    );
    let second = held.another_window(context)?;
    resize_window(context, second, SECOND_WIDTH);
    let second_columns = held.wait(context, |shell, application| {
        let columns = dimensions(shell, application, &key)?;
        let other = dimensions(second.read(application).ok()?, application, &key)?;
        (columns > wider && other == columns).then_some(columns)
    })?;
    resize_window(context, held.window, FINAL_WIDTH);
    let final_columns = held.wait(context, |shell, application| {
        let columns = dimensions(shell, application, &key)?;
        let other = dimensions(second.read(application).ok()?, application, &key)?;
        (columns > second_columns && other == columns).then_some(columns)
    })?;
    assert!(
        final_columns > second_columns,
        "last real window resize controls both native surfaces"
    );
    Ok(())
}

/// Model and native dimensions agree only once both delta and VT resize routing completed.
fn dimensions(shell: &WindowShell, application: &gpui_kit::App, key: &PaneKey) -> Option<u16> {
    let pane = shell
        .hosts()
        .state()
        .model()
        .host(&key.host)?
        .model
        .sessions
        .iter()
        .flat_map(|session| &session.tabs)
        .flat_map(|tab| &tab.panes)
        .find(|pane| pane.id == key.pane)?;
    let snapshot = shell
        .surface(key)?
        .read(application)
        .grid()
        .read(application)
        .snapshot()?;
    (snapshot.columns == pane.columns && snapshot.rows.len() == usize::from(pane.rows))
        .then_some(pane.columns)
}

/// Resize a real headless platform window; production prepaint submits the measured cell count.
fn resize_window(context: &mut TestAppContext, window: WindowHandle<WindowShell>, width: f32) {
    context.simulate_window_resize(
        window.into(),
        gpui_kit::size(gpui_kit::px(width), gpui_kit::px(WINDOW_HEIGHT)),
    );
}

/// Each independent client has one engine and one native terminal owner.
///
/// # Errors
/// Returns native terminal startup failures.
fn open_window(
    context: &mut TestAppContext,
    bridge: iznik_app::bridge::EngineBridge,
) -> Result<WindowHandle<WindowShell>, Failed> {
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    Ok(context.add_window(|window, context| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ..ShellOptions::default()
            },
            window,
            context,
        )
    }))
}

#[path = "support/credit.rs"]
mod credit;
