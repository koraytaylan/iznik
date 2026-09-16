//! Model-driven pane lifetime and event routing for the application window.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, ElementExt};
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    ParentElement, Render, Styled, Subscription, Task, Window, div, px,
};
use gpui_kit::{
    InteractiveElement, Pixels, SharedString, Size, StatefulInteractiveElement, TestSupportExt,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::HostState;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{SessionId, TabId};
use iznik_protocol::model::{LayoutNode, Tab};

use crate::actions::ActionId;
use crate::bars;
use crate::bridge::{EngineBridge, EngineEvent};
use crate::grid::GridMetrics;
use crate::host_ui::{HostUi, Notice, NoticeKind};
use crate::palette::{self, Palette};
use crate::settings::{Settings, Watcher};
use crate::splits;
use crate::surface::{PaneSurface, SurfaceFailure};
use crate::theme::{AppTheme, terminal_theme};
use crate::vt::{PaneKey, TerminalTheme, VtCommand, VtThread};

/// A short main-thread update cadence reads owned channels without blocking drawing.
const UPDATE_INTERVAL: Duration = Duration::from_millis(16);
/// Bound each owner drain so continuous terminal output cannot monopolize a UI update.
const MAXIMUM_EVENTS_PER_UPDATE: usize = 256;
/// Default width used when the palette creates a new pane or session.
const DEFAULT_COLUMNS: u16 = 80;
/// Default height used when the palette creates a new pane or session.
const DEFAULT_ROWS: u16 = 24;
/// Default session name used by the argument-free palette action.
const DEFAULT_SESSION_NAME: &str = "session";
/// Default tab name used by the argument-free palette action.
const DEFAULT_TAB_NAME: &str = "tab";

/// Window options whose timing can be shortened or disabled by a headless caller.
#[derive(Clone, Debug)]
pub struct ShellOptions {
    /// Channel polling cadence; `None` lets a host application call `update` itself.
    pub update_interval: Option<Duration>,
    /// Maximum ready messages read from each owner in one update.
    pub maximum_events_per_update: usize,
    /// Initial terminal typography and cell geometry.
    pub metrics: GridMetrics,
    /// Native terminal defaults supplied when a screen creates its emulator.
    pub theme: TerminalTheme,
    /// Optional settings file polled during updates.
    pub settings_path: Option<PathBuf>,
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            update_interval: Some(UPDATE_INTERVAL),
            maximum_events_per_update: MAXIMUM_EVENTS_PER_UPDATE,
            metrics: GridMetrics::default(),
            theme: TerminalTheme::default(),
            settings_path: None,
        }
    }
}

/// Stable identity of a selected tab across hosts and sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabKey {
    /// Host alias that owns the session.
    pub host: HostId,
    /// Session containing the tab.
    pub session: SessionId,
    /// The selected tab within that session.
    pub tab: TabId,
}

/// One persistent pane view and the window subscriptions attached to it.
#[derive(Debug)]
struct HeldPane {
    /// Retained while the pane exists, including transport loss and layout changes.
    surface: Entity<PaneSurface>,
    /// Whether this window has successfully requested a subscription.
    subscribed: bool,
    /// Last measured cell geometry successfully submitted to the engine.
    measured: Option<(u16, u16)>,
    /// Pending authoritative resize, preventing duplicate owner requests before its reply.
    native_size: Option<(u16, u16)>,
    /// Focus and failure routes remain alive with the pane.
    _subscriptions: Vec<Subscription>,
}

/// The one window's model, selection, terminal surfaces and shared owner channels.
#[derive(Debug)]
pub struct WindowShell {
    /// Sole engine owner and model mirror; the shell does not maintain another reducer.
    hosts: HostUi,
    /// One shared terminal owner for all panes shown by the window.
    thread: Rc<VtThread>,
    /// Stable pane entities, keyed by host as well as pane number.
    panes: BTreeMap<PaneKey, HeldPane>,
    /// The tab whose layout is currently visible.
    selected: Option<TabKey>,
    /// Latest authoritative tree for that tab.
    layout: Option<LayoutNode>,
    /// Changes only when the selected tree changes, resetting kit divider state.
    revision: u64,
    /// Terminal defaults and update cadence.
    options: ShellOptions,
    /// Validated settings retained by this shell.
    settings: Settings,
    /// Optional watcher for the configured settings file.
    settings_watcher: Option<Watcher>,
    /// Periodic pump is cancelled when the shell drops.
    _update_task: Option<Task<()>>,
    /// Latest local routing failure, dismissible without discarding host state.
    last_failure: Option<Notice>,
    /// Transient command palette state rendered over the shell.
    pub(crate) palette: Palette,
    /// Baseline window focus, held until a pane claims it, so shortcuts such
    /// as opening the palette work before any session exists.
    focus_handle: FocusHandle,
}

impl Focusable for WindowShell {
    fn focus_handle(&self, _context: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WindowShell {
    /// Assemble a shell over owners whose startup errors the caller already handled.
    pub fn new(
        bridge: EngineBridge,
        thread: Rc<VtThread>,
        options: ShellOptions,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> Self {
        let update_task = options.update_interval.map(|interval| {
            context.spawn_in(window, async move |shell, asynchronous| {
                loop {
                    asynchronous.background_executor().timer(interval).await;
                    if shell
                        .update_in(asynchronous, |shell, target_window, update_context| {
                            shell.update(target_window, update_context);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        let settings_watcher = options.settings_path.clone().map(Watcher::new);
        let focus_handle = context.focus_handle();
        window.focus(&focus_handle, context);
        Self {
            hosts: HostUi::new(bridge),
            thread,
            panes: BTreeMap::new(),
            selected: None,
            layout: None,
            revision: 0,
            options,
            settings: Settings::default(),
            settings_watcher,
            _update_task: update_task,
            last_failure: None,
            palette: Palette::default(),
            focus_handle,
        }
    }
    /// The shared host interface used by tabs, sessions and application actions.
    #[must_use]
    pub fn hosts(&self) -> &HostUi {
        &self.hosts
    }
    /// The same interface for action submission and draining displayed notices.
    pub fn hosts_mut(&mut self) -> &mut HostUi {
        &mut self.hosts
    }
    /// Submit a session command through the shell's sole engine bridge.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the host is stopped or refuses admission.
    pub fn dispatch_command(
        &mut self,
        alias: &str,
        command: SessionCommand,
    ) -> Result<iznik_client::commands::Submission, crate::bridge::EngineError> {
        self.hosts.command(alias, command)
    }
    /// Dispatch an inventory action using the current model selection.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the selected host is stopped or refuses
    /// the command.
    pub fn dispatch_action(
        &mut self,
        action: ActionId,
    ) -> Result<bool, crate::bridge::EngineError> {
        let selected = self.selected.clone();
        let host = selected.as_ref().map(|key| key.host.0.clone()).or_else(|| {
            self.hosts
                .state()
                .hosts()
                .next()
                .map(|(host, _)| host.0.clone())
        });
        let Some(host) = host else {
            return Ok(false);
        };
        match action {
            ActionId::RemoveHost => {
                self.hosts.remove_host(&host)?;
                return Ok(true);
            }
            ActionId::ReconnectHost => {
                self.hosts.reconnect(&host)?;
                return Ok(true);
            }
            ActionId::UpgradeHost => {
                self.hosts.upgrade(&host, false)?;
                return Ok(true);
            }
            ActionId::UninstallHost => {
                self.hosts.uninstall(&host)?;
                return Ok(true);
            }
            ActionId::AddHost => return Ok(false),
            _ => {}
        }
        let Some(command) = self.command_for_action(action, selected.as_ref()) else {
            return Ok(false);
        };
        self.dispatch_command(&host, command).map(|_| true)
    }
    /// Translate an argument-free action into a command using current state.
    fn command_for_action(
        &self,
        action: ActionId,
        selected: Option<&TabKey>,
    ) -> Option<SessionCommand> {
        match action {
            ActionId::CreateSession => Some(SessionCommand::CreateSession {
                name: DEFAULT_SESSION_NAME.to_owned(),
                columns: DEFAULT_COLUMNS,
                rows: DEFAULT_ROWS,
                working_directory: None,
            }),
            ActionId::CloseTab => selected.map(|key| bars::close_tab(key.tab)),
            ActionId::CloseSession => selected.map(|key| bars::close_session(key.session)),
            ActionId::SetLayout => Some(SessionCommand::SetLayout {
                tab: selected?.tab,
                layout: self.layout.clone()?,
            }),
            ActionId::CreateTab => Some(SessionCommand::CreateTab {
                session: selected?.session,
                name: DEFAULT_TAB_NAME.to_owned(),
                columns: DEFAULT_COLUMNS,
                rows: DEFAULT_ROWS,
                working_directory: None,
            }),
            ActionId::CreatePane => {
                let key = selected?;
                let view = self.hosts.state().model().host(&key.host)?;
                let target = view.focus.or_else(|| {
                    self.tab(key)
                        .and_then(|tab| tab.panes.first().map(|pane| pane.id))
                })?;
                Some(splits::split_command(
                    key.tab,
                    target,
                    iznik_protocol::model::SplitDirection::Horizontal,
                    false,
                    DEFAULT_COLUMNS,
                    DEFAULT_ROWS,
                ))
            }
            ActionId::ClosePane => Some(SessionCommand::ClosePane {
                pane: self.hosts.state().model().host(&selected?.host)?.focus?,
            }),
            ActionId::RenameSession
            | ActionId::RenameTab
            | ActionId::ReorderTabs
            | ActionId::MovePane
            | ActionId::AddHost
            | ActionId::RemoveHost
            | ActionId::ReconnectHost
            | ActionId::UpgradeHost
            | ActionId::UninstallHost => None,
        }
    }
    /// The currently selected host-qualified tab.
    #[must_use]
    pub fn selected(&self) -> Option<&TabKey> {
        self.selected.as_ref()
    }
    /// Apply application theme defaults to the shell and every retained emulator.
    pub fn apply_theme(&mut self, theme: &AppTheme, context: &mut Context<'_, Self>) {
        let terminal = terminal_theme(theme);
        self.options.theme = terminal.clone();
        let keys: Vec<_> = self.panes.keys().cloned().collect();
        for key in keys {
            if let Err(error) = self.thread.send(VtCommand::Theme {
                key: key.clone(),
                theme: Box::new(terminal.clone()),
            }) {
                self.failure(&key.host, error.to_string(), context);
            }
        }
        context.notify();
    }
    /// Poll a settings watcher and hot-apply a changed validated theme.
    ///
    /// # Errors
    ///
    /// Returns a field-specific settings error while retaining the current theme.
    pub fn reload_settings(
        &mut self,
        watcher: &mut Watcher,
        settings: &mut Settings,
        context: &mut Context<'_, Self>,
    ) -> Result<bool, crate::settings::SettingsError> {
        let changed = watcher.reload(settings)?;
        if changed {
            let theme = settings.theme.clone();
            self.apply_theme(&theme, context);
        }
        Ok(changed)
    }
    /// Poll the configured settings file during the ordinary update cycle.
    fn poll_settings(&mut self, context: &mut Context<'_, Self>) {
        let Some(watcher) = self.settings_watcher.as_mut() else {
            return;
        };
        match watcher.reload(&mut self.settings) {
            Ok(true) => {
                let theme = self.settings.theme.clone();
                self.apply_theme(&theme, context);
            }
            Ok(false) => {}
            Err(refusal) => {
                self.last_failure = Some(Notice {
                    host: HostId("settings".to_owned()),
                    kind: NoticeKind::Failure,
                    detail: format!("{}: {}", refusal.field, refusal.message),
                });
                context.notify();
            }
        }
    }
    /// A retained surface, also available while its host is reconnecting.
    #[must_use]
    pub fn surface(&self, key: &PaneKey) -> Option<&Entity<PaneSurface>> {
        self.panes.get(key).map(|held| &held.surface)
    }
    /// Select an existing tab; a stale action leaves the current selection untouched.
    pub fn select(
        &mut self,
        key: TabKey,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> bool {
        if self.tab(&key).is_none() {
            return false;
        }
        self.selected = Some(key);
        self.reconcile(window, context);
        true
    }
    /// Select the adjacent tab from the model and reconcile its visible panes.
    pub fn select_next_tab(
        &mut self,
        forward: bool,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> bool {
        let Some(key) = bars::next_tab(self.hosts.state(), self.selected.as_ref(), forward) else {
            return false;
        };
        self.select(key, window, context)
    }
    /// Close the selected tab through the same command path as the palette.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the selected host is stopped or refuses
    /// the command.
    pub fn close_selected_tab(
        &mut self,
    ) -> Result<Option<iznik_client::commands::Submission>, crate::bridge::EngineError> {
        let Some(selected) = self.selected.as_ref() else {
            return Ok(None);
        };
        let host = selected.host.0.clone();
        let command = bars::close_tab(selected.tab);
        self.dispatch_command(&host, command).map(Some)
    }
    /// Handle the keyboard-first tab operations while the shell is focused.
    pub fn bar_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> bool {
        if !event.keystroke.modifiers.control {
            return false;
        }
        match event.keystroke.key.as_str() {
            "tab" => {
                let _selected =
                    self.select_next_tab(!event.keystroke.modifiers.shift, window, context);
                true
            }
            "w" => {
                let _submission = self.close_selected_tab();
                true
            }
            _ => false,
        }
    }
    /// Submit a divider drag against the selected tab's authoritative layout.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the selected host is stopped or refuses
    /// the resulting layout.
    pub fn drag_selected_layout(
        &mut self,
        divider: usize,
        delta: f32,
        extent: f32,
    ) -> Result<Option<iznik_client::commands::Submission>, crate::bridge::EngineError> {
        let Some(selected) = self.selected.as_ref() else {
            return Ok(None);
        };
        let Some(layout) = self.layout.as_ref() else {
            return Ok(None);
        };
        let Some(command) = splits::drag_command(selected.tab, layout, divider, delta, extent)
        else {
            return Ok(None);
        };
        let host = selected.host.0.clone();
        self.dispatch_command(&host, command).map(Some)
    }
    /// Submit an equalize request for the selected tab's root split.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the selected host is stopped or refuses
    /// the resulting layout.
    pub fn equalize_selected_layout(
        &mut self,
    ) -> Result<Option<iznik_client::commands::Submission>, crate::bridge::EngineError> {
        let Some(selected) = self.selected.as_ref() else {
            return Ok(None);
        };
        let Some(layout) = self.layout.as_ref() else {
            return Ok(None);
        };
        let Some(command) = splits::equalize_command(selected.tab, layout) else {
            return Ok(None);
        };
        let host = selected.host.0.clone();
        self.dispatch_command(&host, command).map(Some)
    }
    /// Drain ready messages on the GPUI thread without waiting on either owner.
    pub fn update(&mut self, window: &mut Window, context: &mut Context<'_, Self>) {
        for _event in 0..self.options.maximum_events_per_update {
            let Some(event) = self.hosts.bridge().poll() else {
                break;
            };
            self.absorb(event, window, context);
        }
        for _event in 0..self.options.maximum_events_per_update {
            let Some(event) = self.thread.poll() else {
                break;
            };
            if let Some(held) = self.panes.get(&event.key) {
                let key = event.key.clone();
                let result = held.surface.update(context, |surface, context| {
                    surface.receive(event, self.hosts.bridge(), context)
                });
                if let Err(error) = result {
                    self.failure(&key.host, error.to_string(), context);
                }
            }
        }
        self.synchronize_sizes(context);
        self.poll_settings(context);
    }
    /// Route terminal payloads before applying their accompanying model/lifecycle event.
    /// This same entry point lets headless tests provide authoritative model messages.
    pub fn absorb(
        &mut self,
        event: EngineEvent,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        if let EngineEvent::Said(said) = &event {
            let identity = match said {
                ManagerEvent::Screen { host, pane, .. }
                | ManagerEvent::Bytes { host, pane, .. } => Some(PaneKey {
                    host: host.clone(),
                    pane: *pane,
                }),
                _ => None,
            };
            if let Some(key) = identity
                && self.panes.contains_key(&key)
                && let Err(error) =
                    EngineBridge::feed_terminal(&self.thread, said, &self.options.theme)
            {
                self.failure(&key.host, error.to_string(), context);
            }
        }
        self.hosts.absorb_event(event);
        self.reconcile(window, context);
        context.notify();
    }
    /// Find a tab only in the engine's reconciled model.
    fn tab(&self, key: &TabKey) -> Option<&Tab> {
        self.hosts
            .state()
            .model()
            .host(&key.host)?
            .model
            .sessions
            .iter()
            .find(|session| session.id == key.session)?
            .tabs
            .iter()
            .find(|tab| tab.id == key.tab)
    }
    /// Choose the first existing tab when the previous selection disappeared.
    fn first_tab(&self) -> Option<TabKey> {
        self.hosts
            .state()
            .model()
            .hosts
            .iter()
            .find_map(|(host, view)| {
                view.model.sessions.iter().find_map(|session| {
                    session.tabs.first().map(|tab| TabKey {
                        host: host.clone(),
                        session: session.id,
                        tab: tab.id,
                    })
                })
            })
    }
    /// Keep live entities through tree changes; only model removal destroys an emulator.
    fn reconcile(&mut self, window: &mut Window, context: &mut Context<'_, Self>) {
        if self
            .selected
            .as_ref()
            .is_none_or(|selected| self.tab(selected).is_none())
        {
            self.selected = self.first_tab();
        }
        let next_layout = self
            .selected
            .as_ref()
            .and_then(|key| self.tab(key))
            .map(|tab| tab.layout.clone());
        if self.layout != next_layout {
            self.layout = next_layout;
            self.revision = self.revision.saturating_add(1);
            context.notify();
        }
        let wanted: BTreeSet<_> = self
            .selected
            .as_ref()
            .and_then(|selected| {
                self.layout.as_ref().map(|layout| {
                    layout
                        .leaves()
                        .into_iter()
                        .map(|pane| PaneKey {
                            host: selected.host.clone(),
                            pane,
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
        for key in &wanted {
            if !self.panes.contains_key(key) {
                let held = self.create_pane(key, window, context);
                self.panes.insert(key.clone(), held);
            }
        }
        self.attach_visible(&wanted, context);
        self.remove_missing(context);
    }
    /// Build one cached surface and retain its focus/failure observers.
    fn create_pane(
        &self,
        key: &PaneKey,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> HeldPane {
        let surface = context.new(|context| {
            PaneSurface::new(
                key.clone(),
                self.options.metrics.clone(),
                Rc::clone(&self.thread),
                context,
            )
        });
        let focus_key = key.clone();
        let focus = context.on_focus_in(
            &surface.read(context).focus_handle(context),
            window,
            move |shell, _, context| {
                if let Err(error) = shell
                    .hosts
                    .bridge()
                    .focus(&focus_key.host.0, Some(focus_key.pane))
                {
                    shell.failure(&focus_key.host, error.to_string(), context);
                }
            },
        );
        let failure = context.subscribe(&surface, |shell, _, failure: &SurfaceFailure, context| {
            shell.failure(&failure.key.host, failure.detail.clone(), context);
        });
        HeldPane {
            surface,
            subscribed: false,
            measured: None,
            native_size: None,
            _subscriptions: vec![focus, failure],
        }
    }
    /// Subscribe on appearance and unsubscribe on hiding, without repeating successful orders.
    fn attach_visible(&mut self, wanted: &BTreeSet<PaneKey>, context: &mut Context<'_, Self>) {
        let mut failures = Vec::new();
        for (key, held) in &mut self.panes {
            let visible = wanted.contains(key);
            if held.subscribed == visible {
                continue;
            }
            let result = if visible {
                self.hosts.bridge().subscribe(&key.host.0, key.pane)
            } else {
                self.hosts.bridge().unsubscribe(&key.host.0, key.pane)
            };
            match result {
                Ok(()) => held.subscribed = visible,
                Err(error) => failures.push((key.host.clone(), error.to_string())),
            }
        }
        for (host, detail) in failures {
            self.failure(&host, detail, context);
        }
    }
    /// Retire surfaces only when their pane disappears from the complete host model.
    fn remove_missing(&mut self, context: &mut Context<'_, Self>) {
        let existing: BTreeSet<_> = self
            .hosts
            .state()
            .model()
            .hosts
            .iter()
            .flat_map(|(host, view)| {
                view.model.sessions.iter().flat_map(move |session| {
                    session.tabs.iter().flat_map(move |tab| {
                        tab.panes.iter().map(move |pane| PaneKey {
                            host: host.clone(),
                            pane: pane.id,
                        })
                    })
                })
            })
            .collect();
        let removed: Vec<_> = self
            .panes
            .keys()
            .filter(|key| !existing.contains(*key))
            .cloned()
            .collect();
        for key in removed {
            self.panes.remove(&key);
            if let Err(error) = self.thread.send(VtCommand::Close(key.clone())) {
                self.failure(&key.host, error.to_string(), context);
            }
        }
    }
    /// Apply authoritative model dimensions on the VT owner after its initial screen exists.
    fn synchronize_sizes(&mut self, context: &mut Context<'_, Self>) {
        let mut failures = Vec::new();
        for (host, view) in &self.hosts.state().model().hosts {
            for pane in view
                .model
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .flat_map(|tab| &tab.panes)
            {
                let key = PaneKey {
                    host: host.clone(),
                    pane: pane.id,
                };
                let Some(held) = self.panes.get_mut(&key) else {
                    continue;
                };
                let Some(snapshot) = held.surface.read(context).grid().read(context).snapshot()
                else {
                    continue;
                };
                let desired = (pane.columns, pane.rows);
                if snapshot.columns == pane.columns && snapshot.rows.len() == usize::from(pane.rows)
                {
                    held.native_size = None;
                } else if held.native_size != Some(desired) {
                    match self.thread.send(VtCommand::Resize {
                        key,
                        columns: pane.columns,
                        rows: pane.rows,
                    }) {
                        Ok(()) => held.native_size = Some(desired),
                        Err(error) => failures.push((host.clone(), error.to_string())),
                    }
                }
            }
        }
        for (host, detail) in failures {
            self.failure(&host, detail, context);
        }
    }
    /// Submit changed visible geometry once; a remote resize does not cause a size fight.
    fn measured(&mut self, key: &PaneKey, size: Size<Pixels>, context: &mut Context<'_, Self>) {
        let Some(columns) = cells(size.width, self.options.metrics.cell_width) else {
            return;
        };
        let Some(rows) = cells(size.height, self.options.metrics.line_height) else {
            return;
        };
        let Some(held) = self.panes.get_mut(key) else {
            return;
        };
        if held.measured == Some((columns, rows)) {
            return;
        }
        match self
            .hosts
            .bridge()
            .resize(&key.host.0, key.pane, columns, rows)
        {
            Ok(()) => held.measured = Some((columns, rows)),
            Err(error) => self.failure(&key.host, error.to_string(), context),
        }
    }
    /// Deliver a pane routing failure through the window's existing notice inventory.
    pub(crate) fn failure(
        &mut self,
        host: &HostId,
        detail: String,
        context: &mut Context<'_, Self>,
    ) {
        let notice = Notice {
            host: host.clone(),
            kind: NoticeKind::Failure,
            detail,
        };
        if self.last_failure.as_ref() != Some(&notice) {
            self.last_failure = Some(notice.clone());
            context.emit(notice);
            context.notify();
        }
    }
}
impl gpui_kit::EventEmitter<Notice> for WindowShell {}
impl WindowShell {
    /// Build the callback that turns a native root-divider resize into a command.
    fn resize_callback(
        selected: &TabKey,
        layout: &LayoutNode,
        entity: &gpui_kit::WeakEntity<Self>,
    ) -> crate::layout::ResizeCallback {
        let selected = selected.clone();
        let layout = layout.clone();
        let entity = entity.clone();
        Rc::new(move |state, _window, application| {
            let sizes = state.read(application).sizes().clone();
            let Some(first) = sizes.first().copied() else {
                return;
            };
            let Some(second) = sizes.get(1).copied() else {
                return;
            };
            let extent = f32::from(first) + f32::from(second);
            let LayoutNode::Split { children, .. } = &layout else {
                return;
            };
            let Some(left) = children.first() else {
                return;
            };
            let Some(right) = children.get(1) else {
                return;
            };
            let weight_total = left.weight.saturating_add(right.weight);
            if weight_total == 0 {
                return;
            }
            let Ok(left_weight) = u16::try_from(left.weight) else {
                return;
            };
            let Ok(total_weight) = u16::try_from(weight_total) else {
                return;
            };
            let expected = extent * (f32::from(left_weight) / f32::from(total_weight));
            let delta = f32::from(first) - expected;
            if let Some(command) = splits::drag_command(selected.tab, &layout, 0, delta, extent) {
                let alias = selected.host.0.clone();
                let _updated = entity.update(application, |shell, context| {
                    if let Err(error) = shell.dispatch_command(&alias, command) {
                        shell.failure(&selected.host, error.to_string(), context);
                    }
                });
            }
        })
    }
}
impl Render for WindowShell {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = context.theme();
        let entity = context.entity().downgrade();
        let body = match (&self.selected, &self.layout) {
            (Some(selected), Some(layout)) => splits::render_interactive(
                layout,
                self.revision,
                |pane| {
                    let key = PaneKey {
                        host: selected.host.clone(),
                        pane,
                    };
                    let Some(held) = self.panes.get(&key) else {
                        return div().into_any_element();
                    };
                    let entity = entity.clone();
                    div()
                        .size_full()
                        .overflow_hidden()
                        .child(held.surface.clone())
                        .on_prepaint(move |bounds, window, application| {
                            let entity = entity.clone();
                            let key = key.clone();
                            window.defer(application, move |_, application| {
                                let _updated =
                                    entity.update(application, |shell, update_context| {
                                        shell.measured(&key, bounds.size, update_context);
                                    });
                            });
                        })
                        .into_any_element()
                },
                Self::resize_callback(selected, layout, &entity),
            ),
            _ => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("No sessions")
                .into_any_element(),
        };
        let bars = bars::render(
            theme,
            self.hosts.state(),
            self.selected.as_ref(),
            Some(&entity),
        );
        let palette_overlay =
            palette::render(theme, self.hosts.state(), &self.palette, Some(&entity));
        div()
            .id("window-shell")
            .test_support()
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .on_key_down(context.listener(|shell, event, window, context| {
                let routed = palette::route_key(shell, event, context);
                if routed || shell.bar_key(event, window, context) {
                    context.stop_propagation();
                }
            }))
            .bg(theme.background)
            .text_color(theme.foreground)
            .children(self.banners(context))
            .child(bars.top)
            .child(
                div()
                    .id("pane-area")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(body),
            )
            .child(bars.bottom)
            .child(palette_overlay)
    }
}
impl WindowShell {
    /// Kit alerts show classified engine states; reconnect uses the existing engine operation.
    fn banners(&self, context: &mut Context<'_, Self>) -> Vec<gpui_kit::AnyElement> {
        let mut banners = Vec::new();
        for (host, report) in self.hosts.state().hosts() {
            if matches!(report.connection, HostState::Connected { .. }) {
                continue;
            }
            let host = host.clone();
            let message = format!("{host}: {}", report.connection);
            let label = message.clone();
            let banner_id = SharedString::from(format!("host-banner-{}", host.0));
            let alert = if matches!(report.connection, HostState::Failed { .. }) {
                Alert::error(SharedString::from(format!("host-{}", host.0)), message)
            } else {
                Alert::info(SharedString::from(format!("host-{}", host.0)), message)
            };
            let reconnect = Button::new(SharedString::from(format!("reconnect-{}", host.0)))
                .label("Reconnect")
                .on_click(context.listener(move |shell, _, _, context| {
                    if let Err(error) = shell.hosts.bridge().reconnect(&host.0) {
                        shell.failure(&host, error.to_string(), context);
                    }
                }));
            banners.push(
                div()
                    .id(banner_id)
                    .test_support()
                    .aria_label(label)
                    .flex()
                    .child(alert.banner())
                    .child(reconnect)
                    .into_any_element(),
            );
        }
        if let Some(failure) = &self.last_failure {
            banners.push(
                div()
                    .id("surface-failure")
                    .test_support()
                    .child(
                        Alert::error(
                            "surface-failure-alert",
                            format!("{}: {}", failure.host, failure.detail),
                        )
                        .banner()
                        .on_close(context.listener(
                            |shell, _, _, context| {
                                shell.last_failure = None;
                                context.notify();
                            },
                        )),
                    )
                    .into_any_element(),
            );
        }
        banners
    }
}
/// Convert positive finite surface geometry to complete cells within the protocol range.
fn cells(extent: Pixels, cell: Pixels) -> Option<u16> {
    let extent = f32::from(extent);
    let cell = f32::from(cell);
    if !extent.is_finite() || !cell.is_finite() || extent <= 0.0 || cell <= 0.0 {
        return None;
    }
    let count = extent.div_euclid(cell).clamp(1.0, f32::from(u16::MAX));
    u16::try_from(usize::from(px(count))).ok()
}
