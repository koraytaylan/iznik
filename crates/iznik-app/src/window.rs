//! Model-driven pane lifetime and event routing for the application window.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::component::{ActiveTheme, TitleBar};
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, Role, SharedString, Styled, Subscription, Task,
    TestSupportExt, Window, div, px,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::reduce::Notification;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{SessionId, TabId};
use iznik_protocol::model::{LayoutNode, Session, Tab};

use crate::actions::ActionId;
use crate::bars;
use crate::bridge::{EngineBridge, EngineEvent};
use crate::follow::{self, Following};
use crate::grid::{GridMetrics, measure_cell};
use crate::host_ui::{HostUi, Notice, NoticeKind};
use crate::navigation::{self, ShortcutHint};
use crate::palette::{self, Palette};
use crate::settings::{Settings, Watcher};
use crate::splits;
use crate::status;
use crate::subscription::{self, Subscriptions};
use crate::surface::{PaneSurface, PasteConfirmation, SurfaceFailure};
use crate::tab_label::DEFAULT_TAB_NAME;
use crate::theme::{self, AppTheme};
use crate::vt::{PaneKey, TerminalTheme, VtCommand, VtThread};

/// The pump's slow fallback: owners wake the window when they queue work, so
/// this only bounds how late a missed wakeup or a settings poll can be.
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the settings file is looked at for a change made outside the
/// window. Each look is a file-system stat, so it is kept well apart.
const SETTINGS_INTERVAL: Duration = Duration::from_secs(2);
/// Bound each owner drain so continuous terminal output cannot monopolize a UI update.
const MAXIMUM_EVENTS_PER_UPDATE: usize = 256;
/// How long the session-tabs record waits after a change before it is written.
const SELECTION_WRITE_DELAY: Duration = Duration::from_secs(1);
/// Default width used when the palette creates a new pane or session.
const DEFAULT_COLUMNS: u16 = 80;
/// Default height used when the palette creates a new pane or session.
const DEFAULT_ROWS: u16 = 24;
/// Space between a terminal's text and its pane's edges, in logical pixels:
/// the common inset terminals leave so text does not run into the frame.
pub(crate) const TERMINAL_PADDING: f32 = 8.0;
/// Default session name used by the argument-free palette action.
const DEFAULT_SESSION_NAME: &str = "session";

/// Window options whose timing can be shortened or disabled by a headless caller.
#[derive(Clone, Debug)]
pub struct ShellOptions {
    /// Fallback cadence of the event-driven pump; `None` starts no pump, and
    /// lets a host application call `update` itself.
    pub update_interval: Option<Duration>,
    /// Least time between two looks at the settings file.
    pub settings_interval: Duration,
    /// Maximum ready messages read from each owner in one update.
    pub maximum_events_per_update: usize,
    /// Initial terminal typography and cell geometry.
    pub metrics: GridMetrics,
    /// Native terminal defaults supplied when a screen creates its emulator.
    pub theme: TerminalTheme,
    /// Optional settings file polled during updates and rewritten when they change.
    pub settings_path: Option<PathBuf>,
    /// Where the person's ssh configuration is read and written.
    ///
    /// The product's binary resolves the person's own from `$HOME` and hands
    /// it in; a test hands in a scratch path. The shell itself never reaches
    /// for `$HOME`, so a shell a case builds touches no file this machine
    /// holds, and one given no path reads and writes nothing.
    pub ssh_config_path: Option<PathBuf>,
    /// The file that records the tab each session was left on. Resolved like
    /// [`Self::ssh_config_path`]: absent reads and writes nothing.
    pub selection_path: Option<PathBuf>,
    /// How long a changed record waits, so a burst of tab changes is one write.
    pub selection_write_delay: Duration,
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            update_interval: Some(UPDATE_INTERVAL),
            settings_interval: SETTINGS_INTERVAL,
            maximum_events_per_update: MAXIMUM_EVENTS_PER_UPDATE,
            metrics: GridMetrics::default(),
            theme: TerminalTheme::default(),
            settings_path: None,
            ssh_config_path: None,
            selection_path: None,
            selection_write_delay: SELECTION_WRITE_DELAY,
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

/// Stable identity of a selected session across hosts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionKey {
    /// Host alias that owns the session.
    pub host: HostId,
    /// The selected session.
    pub session: SessionId,
}

/// One persistent pane view and the window subscriptions attached to it.
#[derive(Debug)]
pub(crate) struct HeldPane {
    /// Retained while the pane exists, including transport loss and layout changes.
    pub(crate) surface: Entity<PaneSurface>,
    /// Last measured cell geometry successfully submitted to the engine.
    pub(crate) measured: Option<(u16, u16)>,
    /// Model geometry when a different size was submitted and the model has
    /// not moved since. The local terminal keeps the submission until then.
    pub(crate) awaiting_model: Option<(u16, u16)>,
    /// Pending authoritative resize, preventing duplicate owner requests before its reply.
    pub(crate) native_size: Option<(u16, u16)>,
    /// Focus and failure routes remain alive with the pane.
    _subscriptions: Vec<Subscription>,
}

/// The one window's model, selection, terminal surfaces and shared owner channels.
#[derive(Debug)]
pub struct WindowShell {
    /// Sole engine owner and model mirror; the shell does not maintain another reducer.
    pub(crate) hosts: HostUi,
    /// One shared terminal owner for all panes shown by the window.
    pub(crate) thread: Rc<VtThread>,
    /// Stable pane entities, keyed by host as well as pane number.
    pub(crate) panes: BTreeMap<PaneKey, HeldPane>,
    /// Which panes the hosts are carrying, as they have answered.
    subscriptions: Subscriptions,
    /// The tab whose layout is currently visible.
    pub(crate) selected: Option<TabKey>,
    /// Latest authoritative tree for that tab.
    pub(crate) layout: Option<LayoutNode>,
    /// Changes only when the selected tree changes, resetting kit divider state.
    pub(crate) revision: u64,
    /// Terminal defaults and update cadence.
    pub(crate) options: ShellOptions,
    /// Validated settings retained by this shell.
    pub(crate) settings: Settings,
    /// Optional watcher for the configured settings file.
    pub(crate) settings_watcher: Option<Watcher>,
    /// Event-driven pump, cancelled when the shell drops.
    _update_task: Option<Task<()>>,
    /// Whether the model may have moved since sizes were last put on the
    /// emulators, so output must wait for them.
    pub(crate) sizes_pending: bool,
    /// When the settings file was last looked at; `None` before the first look.
    pub(crate) settings_polled: Option<Instant>,
    /// Latest local routing failure, dismissible without discarding host state.
    pub(crate) last_failure: Option<Notice>,
    /// Transient command palette state rendered over the shell.
    pub(crate) palette: Palette,
    /// The menu a right click opened, while it is open.
    pub(crate) menu: Option<crate::tab_actions::OpenMenu>,
    /// Where the person's ssh configuration is read and written, so a test
    /// points it at its own file instead of the developer's own.
    pub(crate) ssh_config_path: Option<PathBuf>,
    /// Baseline window focus, held until a pane claims it, so shortcuts such
    /// as opening the palette work before any session exists.
    focus_handle: FocusHandle,
    /// What the window is following on a person's behalf.
    pub(crate) following: Following,
    /// The hosts this window has already told a person are offering a server
    /// upgrade, so the toast is raised once per host rather than on every
    /// state the engine says.
    pub(crate) upgrade_notices: BTreeSet<HostId>,
    /// Tabs or sessions numbered while Command is held.
    pub(crate) shortcut_hint: ShortcutHint,
    /// The system's font families, listed once: listing them walks every
    /// installed font, which is too slow to repeat for each theme change.
    installed_fonts: Option<Vec<String>>,
    /// The ssh aliases last read, with the files they were read from.
    pub(crate) ssh_cache: std::cell::RefCell<crate::ssh_config::AliasCache>,
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
        let update_task = options
            .update_interval
            .map(|interval| crate::pump::spawn(&bridge, &thread, interval, window, context));
        let settings_watcher = options.settings_path.clone().map(Watcher::new);
        let focus_handle = context.focus_handle();
        let ssh_config_path = options.ssh_config_path.clone();
        let following = follow::loaded(options.selection_path.as_deref());
        window.focus(&focus_handle, context);
        let mut shell = Self {
            hosts: HostUi::new(bridge),
            thread,
            panes: BTreeMap::new(),
            subscriptions: Subscriptions::new(),
            selected: None,
            layout: None,
            revision: 0,
            options,
            settings: Settings::default(),
            settings_watcher,
            _update_task: update_task,
            settings_polled: None,
            sizes_pending: true,
            last_failure: None,
            palette: Palette::default(),
            menu: None,
            ssh_config_path,
            focus_handle,
            following,
            upgrade_notices: BTreeSet::new(),
            shortcut_hint: ShortcutHint::None,
            installed_fonts: None,
            ssh_cache: std::cell::RefCell::default(),
        };
        crate::settings::load_into(&mut shell, context);
        shell
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
    /// Dispatch an inventory action to the host a new session would go to.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the selected host is stopped or refuses
    /// the command.
    pub fn dispatch_action(
        &mut self,
        action: ActionId,
    ) -> Result<bool, crate::bridge::EngineError> {
        let Some(host) = self.target_host() else {
            return Ok(false);
        };
        self.dispatch_action_on(action, &host)
    }
    /// Dispatch an inventory action, sending a new session to `host` and
    /// everything built from the selection to the selection's host, and
    /// following what it creates.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the host is stopped or refuses the command.
    pub fn dispatch_action_on(
        &mut self,
        action: ActionId,
        host: &HostId,
    ) -> Result<bool, crate::bridge::EngineError> {
        let selected = self.selected.clone();
        match action {
            ActionId::RemoveHost => return self.hosts.remove_host(&host.0).map(|()| true),
            ActionId::ReconnectHost => return self.hosts.reconnect(&host.0).map(|()| true),
            ActionId::UpgradeHost => return self.hosts.upgrade(&host.0, true).map(|()| true),
            ActionId::UninstallHost => return self.hosts.uninstall(&host.0).map(|()| true),
            _ => {}
        }
        let destination = match &selected {
            Some(key) if action != ActionId::CreateSession => key.host.clone(),
            _ => host.clone(),
        };
        let Some(command) = self.command_for_action(action, selected.as_ref(), &destination) else {
            return Ok(false);
        };
        let expected = follow::expectation(action, self.hosts.state(), &destination);
        self.dispatch_command(&destination.0, command)?;
        if expected.is_some() {
            self.following.expected = expected;
        }
        Ok(true)
    }
    /// Translate an argument-free action into a command using current state.
    fn command_for_action(
        &self,
        action: ActionId,
        selected: Option<&TabKey>,
        host: &HostId,
    ) -> Option<SessionCommand> {
        let sessions = self
            .hosts
            .state()
            .model()
            .host(host)
            .map(|view| view.model.sessions.as_slice())
            .unwrap_or_default();
        match action {
            ActionId::CreateSession => Some(SessionCommand::CreateSession {
                name: crate::prompt::numbered_name(
                    DEFAULT_SESSION_NAME,
                    sessions.iter().map(|session| session.name.as_str()),
                ),
                columns: DEFAULT_COLUMNS,
                rows: DEFAULT_ROWS,
                working_directory: None,
            }),
            ActionId::CloseTab => selected.map(|key| bars::close_tab(key.tab)),
            ActionId::CloseSession => selected.map(|key| bars::close_session(key.session)),
            ActionId::CreateTab => Some(SessionCommand::CreateTab {
                session: selected?.session,
                name: crate::prompt::numbered_name(
                    DEFAULT_TAB_NAME,
                    sessions
                        .iter()
                        .filter(|session| Some(session.id) == selected.map(|key| key.session))
                        .flat_map(|session| &session.tabs)
                        .map(|tab| tab.name.as_str()),
                ),
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
            | ActionId::ReorderSessions
            | ActionId::SetLayout
            | ActionId::RenameTab
            | ActionId::ReorderTabs
            | ActionId::MovePane
            | ActionId::AddHost
            | ActionId::RemoveHost
            | ActionId::ReconnectHost
            | ActionId::UpgradeHost
            | ActionId::UninstallHost
            | ActionId::OpenSettings => None,
        }
    }
    /// The currently selected host-qualified tab.
    #[must_use]
    pub fn selected(&self) -> Option<&TabKey> {
        self.selected.as_ref()
    }
    /// Replace the selection without laying it out; the reconcile that
    /// follows does that. The session remembers this tab for the next return.
    pub(crate) fn set_selected(&mut self, key: TabKey) {
        self.remember_shown(&key);
        self.selected = Some(key);
    }
    /// The panes of the visible layout, in reading order.
    pub(crate) fn visible_panes(&self) -> Vec<iznik_protocol::identity::PaneId> {
        self.layout
            .as_ref()
            .map(LayoutNode::leaves)
            .unwrap_or_default()
    }
    /// Replace the shell's appearance settings and apply them everywhere.
    pub fn set_theme(&mut self, theme: AppTheme, context: &mut Context<'_, Self>) {
        self.apply_theme(&theme, context);
        self.settings.theme = theme;
        crate::settings::persist(self, context);
    }
    /// Apply application theme defaults to the shell and every retained emulator.
    /// A size or row height the grid cannot draw is brought into range
    /// first, so no setting can blank the panes or stall their output.
    pub fn apply_theme(&mut self, theme: &AppTheme, context: &mut Context<'_, Self>) {
        let theme = &crate::settings::drawable(theme);
        let terminal = theme::terminal_theme_in(theme, context);
        self.options.theme = terminal.clone();
        let installed = self
            .installed_fonts
            .get_or_insert_with(|| context.text_system().all_font_names());
        let font = SharedString::from(theme::terminal_font(&theme.font_family, installed));
        let size = px(theme.font_size);
        self.options.metrics.font = font.clone();
        self.options.metrics.font_size = size;
        (
            self.options.metrics.cell_width,
            self.options.metrics.line_height,
        ) = measure_cell(context.text_system(), font.clone(), size, theme.line_height);
        theme::apply_chrome_font(context, font, size);
        let metrics = self.options.metrics.clone();
        let option_as_meta = self.settings.option_as_meta;
        for key in self.panes.keys().cloned().collect::<Vec<_>>() {
            if let Err(error) = self.thread.send(VtCommand::Theme {
                key: key.clone(),
                theme: Box::new(terminal.clone()),
            }) {
                self.failure(&key.host, error.to_string(), context);
            }
            if let Some(surface) = self.panes.get(&key).map(|held| held.surface.clone()) {
                surface.update(context, |surface, context| {
                    surface.set_metrics(&metrics, context);
                    surface.set_option_as_meta(option_as_meta, context);
                });
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
            self.settings.clone_from(settings);
            crate::settings::apply_saved(self, context);
        }
        Ok(changed)
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
        self.set_selected(key);
        self.reconcile(None, window, context);
        true
    }
    /// Select the tab last shown in this session, or the one holding the
    /// host's focused pane. A missing session leaves the selection untouched.
    pub fn select_session(
        &mut self,
        key: &SessionKey,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> bool {
        let Some(session) = self.session(key) else {
            return false;
        };
        let remembered = self.following.session_tab.get(key).copied();
        let Some(tab) = bars::shown_tab(self.hosts.state(), &key.host, session, remembered) else {
            return false;
        };
        self.select(
            TabKey {
                host: key.host.clone(),
                session: key.session,
                tab,
            },
            window,
            context,
        )
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
    /// Route terminal payloads before applying their accompanying model/lifecycle event.
    /// This same entry point lets headless tests provide authoritative model messages.
    pub fn absorb(
        &mut self,
        event: EngineEvent,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        // Captured before the event lands: a removed tab is already gone by
        // the time the selection is repaired, and its old place is what says
        // which neighbor to show.
        let place = self
            .selected
            .as_ref()
            .and_then(|selected| bars::tab_place(self.hosts.state(), selected));
        if let EngineEvent::Said(said) = &event {
            self.note_answer(said, context);
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
            {
                // A size already in the model has to be on the local terminal
                // before these bytes, or a redraw is parsed at the old size.
                if self.sizes_pending {
                    self.synchronize_sizes(context);
                }
                if let Err(error) =
                    EngineBridge::feed_terminal(&self.thread, said, &self.options.theme)
                {
                    self.failure(&key.host, error.to_string(), context);
                }
            }
            // Output changes no model and no selection: the grid redraws the
            // rows it changes when its snapshot comes back.
            if matches!(said, ManagerEvent::Bytes { .. }) {
                return;
            }
        }
        self.hosts.absorb_event(event);
        self.sizes_pending = true;
        self.notify_upgrades_on_offer(window, context);
        self.reconcile(place.as_ref(), window, context);
        context.notify();
    }

    /// Tell a person, once per host, when a connected server is offering an
    /// upgrade — because otherwise the only sign is a disabled menu entry.
    fn notify_upgrades_on_offer(&mut self, window: &mut Window, context: &mut Context<'_, Self>) {
        status::notify_upgrades_on_offer(self, window, context);
    }
    /// Find a tab only in the engine's reconciled model.
    pub(crate) fn tab(&self, key: &TabKey) -> Option<&Tab> {
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
    /// Find a session only in the engine's reconciled model.
    pub(crate) fn session(&self, key: &SessionKey) -> Option<&Session> {
        self.hosts
            .state()
            .model()
            .host(&key.host)?
            .model
            .sessions
            .iter()
            .find(|session| session.id == key.session)
    }
    /// Keep live entities through tree changes; only model removal destroys an emulator.
    ///
    /// `place` is the selection as it stood before this reconcile's model
    /// change. When that tab is gone, the next tab of its session takes its
    /// place, and the next session does when the session itself is gone.
    fn reconcile(
        &mut self,
        place: Option<&bars::TabPlace>,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) {
        self.follow_model(context);
        self.settle_selection(place);
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
        let wanted = self.shown_panes();
        for key in &wanted {
            if !self.panes.contains_key(key) {
                let held = self.create_pane(key, window, context);
                self.panes.insert(key.clone(), held);
            }
        }
        self.remove_missing(context);
        self.attach_visible(context);
        self.follow_focus(window, context);
    }
    /// Build one cached surface and retain its focus/failure observers.
    fn create_pane(
        &self,
        key: &PaneKey,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> HeldPane {
        let surface = context.new(|context| {
            let mut surface = PaneSurface::new(
                key.clone(),
                self.options.metrics.clone(),
                Rc::clone(&self.thread),
                context,
            );
            surface.set_option_as_meta(self.settings.option_as_meta, context);
            surface
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
        let leaving_surface = surface.clone();
        let left = context.on_focus_out(
            &surface.read(context).focus_handle(context),
            window,
            move |_, _, _, context| show_focus(&leaving_surface, false, context),
        );
        let focus_surface = surface.clone();
        let shown = context.on_focus_in(
            &surface.read(context).focus_handle(context),
            window,
            move |_, _, context| show_focus(&focus_surface, true, context),
        );
        let failure = context.subscribe(&surface, |shell, _, failure: &SurfaceFailure, context| {
            shell.failure(&failure.key.host, failure.detail.clone(), context);
        });
        let paste = context.subscribe(&surface, |shell, _, paste: &PasteConfirmation, context| {
            shell.confirm_paste(paste, context);
        });
        HeldPane {
            surface,
            measured: None,
            awaiting_model: None,
            native_size: None,
            _subscriptions: vec![focus, failure, paste, left, shown],
        }
    }
    /// The panes of the selected tab's layout, keyed by its host.
    pub(crate) fn shown_panes(&self) -> BTreeSet<PaneKey> {
        let Some(selected) = self.selected.as_ref() else {
            return BTreeSet::new();
        };
        self.visible_panes()
            .into_iter()
            .map(|pane| PaneKey {
                host: selected.host.clone(),
                pane,
            })
            .collect()
    }
    /// Ask the hosts to carry exactly the shown panes: subscribe what is not
    /// carried yet or whose refusal has been waited out, and let go of the rest.
    pub(crate) fn attach_visible(&mut self, context: &mut Context<'_, Self>) {
        let shown = self.shown_panes();
        let orders = self.subscriptions.plan(&shown, Instant::now());
        self.send_orders(&orders, context);
    }
    /// Move each pane's standing on what a host said about carrying it.
    fn note_answer(&mut self, said: &ManagerEvent, context: &mut Context<'_, Self>) {
        match said {
            ManagerEvent::Screen { host, pane, .. } => self.subscriptions.screen(&PaneKey {
                host: host.clone(),
                pane: *pane,
            }),
            ManagerEvent::Detached { host, pane } => self.subscriptions.detached(&PaneKey {
                host: host.clone(),
                pane: *pane,
            }),
            ManagerEvent::Notify(Notification::Refused { host, code, .. }) => {
                let shown = self.shown_panes();
                let orders = self
                    .subscriptions
                    .refused(host, *code, &shown, Instant::now());
                self.send_orders(&orders, context);
            }
            _ => {}
        }
    }
    /// Hand subscription orders to the engine, reporting any it would not take.
    fn send_orders(&mut self, orders: &[subscription::Order], context: &mut Context<'_, Self>) {
        let failures = subscription::send(
            &mut self.subscriptions,
            self.hosts.bridge(),
            orders,
            Instant::now(),
        );
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
            self.subscriptions.forget(&key);
            if let Err(error) = self.thread.send(VtCommand::Close(key.clone())) {
                self.failure(&key.host, error.to_string(), context);
            }
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
/// Draw a pane's cursor as focused or not.
fn show_focus(surface: &Entity<PaneSurface>, focused: bool, context: &mut App) {
    let grid = surface.read(context).grid().clone();
    let _redrawn = grid.update(context, |grid, context| grid.set_focused(focused, context));
}
impl gpui_kit::EventEmitter<Notice> for WindowShell {}
impl Drop for WindowShell {
    fn drop(&mut self) {
        self.write_session_tabs(true);
    }
}
impl Render for WindowShell {
    fn render(
        &mut self,
        root_window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let entity = context.entity().downgrade();
        let body = self.body(&entity, context);
        let theme = context.theme();
        let placement = if self.settings.theme.tabs_in_title_bar {
            bars::TabPlacement::TitleBar
        } else {
            bars::TabPlacement::Bar
        };
        let bars = bars::render_placed(
            theme,
            self.hosts.state(),
            self.selected.as_ref(),
            Some(&entity),
            placement,
            self.shortcut_hint,
        );
        let (title, tab_bar) = match placement {
            bars::TabPlacement::TitleBar => (bars.top, None),
            bars::TabPlacement::Bar => ("iznik".into_any_element(), Some(bars.top)),
        };
        let palette_overlay =
            palette::render(theme, self.hosts.state(), &self.palette, Some(&entity));
        let menu_overlay = self.menu.as_ref().map(crate::tab_actions::render_open);
        crate::menu::attach(
            div()
                .id("window-shell")
                .test_support()
                .role(Role::Application)
                .aria_label("iznik")
                .track_focus(&self.focus_handle)
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .capture_key_down(context.listener(|shell, event, window, context| {
                    if palette::route_key(shell, event, window, context)
                        || shell.chord(&event.keystroke, window, context)
                        || shell.shortcut_key(event, window, context)
                        || shell.bar_key(event, window, context)
                    {
                        context.stop_propagation();
                    }
                    navigation::note(shell, event.keystroke.modifiers, context);
                }))
                .on_modifiers_changed(navigation::on_modifiers(context))
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(TitleBar::new().child(title))
                .children(tab_bar)
                .child(
                    div()
                        .id("pane-area")
                        .test_support()
                        .role(Role::Group)
                        .aria_label("Panes")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .bg(crate::chrome::painted_pane_color(&self.options.theme))
                        .child(body)
                        // Over the panes, not above them: a strip that comes
                        // and goes must not resize every terminal twice.
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .w_full()
                                .children(self.connection_strips(context)),
                        ),
                )
                .child(bars.bottom)
                .child(palette_overlay)
                .children(menu_overlay)
                .children(gpui_kit::component::Root::render_notification_layer(
                    root_window,
                    context,
                )),
            context,
        )
    }
}
