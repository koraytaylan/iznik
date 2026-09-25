//! The window following what a person just asked for: the host they added is
//! the one the stage describes and gets a first session when it connects
//! empty, the tab they created is the one they see, and the pane they see is
//! the one they type into.
//!
//! Commands are answered by model changes that arrive later, so each request
//! leaves an expectation behind and the reconcile that sees the model change
//! is what acts on it.

use std::collections::{BTreeMap, BTreeSet};

use gpui_kit::{Context, Focusable, Window};
use iznik_client::commands::Submission;
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{PaneId, TabId};

use crate::actions::ActionId;
use crate::bars::{self, TabPlace};
use crate::bridge::EngineError;
use crate::host_ui::EngineState;
use crate::session_tabs::{self, SessionTabs};
use crate::status::Remedy;
use crate::vt::PaneKey;
use crate::window::{SessionKey, TabKey, WindowShell};

/// A model change this window asked for and has not seen yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expectation {
    /// A tab will appear on the host; select it.
    Tab {
        /// The host it will appear on.
        host: HostId,
        /// Every tab the host had when it was asked for.
        known: BTreeSet<TabId>,
    },
    /// A pane will appear on the host; focus it.
    Pane {
        /// The host it will appear on.
        host: HostId,
        /// Every pane the host had when it was asked for.
        known: BTreeSet<PaneId>,
    },
}

/// What the window is following on a person's behalf.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Following {
    /// The host added most recently from this window, which the stage
    /// describes while no tab is visible.
    pub preferred: Option<HostId>,
    /// Hosts added from this window that get a first session when they
    /// connect holding none.
    pub starting: BTreeSet<HostId>,
    /// The change asked for most recently and not yet seen.
    pub expected: Option<Expectation>,
    /// The pane to give keyboard focus once its surface exists.
    pub focus: Option<PaneKey>,
    /// The tab whose pane was last given keyboard focus.
    pub focused_tab: Option<TabKey>,
    /// The tab last shown in each session. Returning to a session opens this
    /// tab while it still exists.
    pub session_tab: BTreeMap<SessionKey, TabId>,
    /// The tab that was on screen. The next launch opens it while it exists.
    pub open: Option<TabKey>,
    /// When the record first changed without being written since; a burst of
    /// tab changes is written once, after the selection write delay.
    pub unwritten_since: Option<std::time::Instant>,
}

/// Every tab a host holds.
fn tabs(state: &EngineState, host: &HostId) -> BTreeSet<TabId> {
    state
        .model()
        .host(host)
        .into_iter()
        .flat_map(|view| &view.model.sessions)
        .flat_map(|session| &session.tabs)
        .map(|tab| tab.id)
        .collect()
}

/// Every pane a host holds.
fn panes(state: &EngineState, host: &HostId) -> BTreeSet<PaneId> {
    state
        .model()
        .host(host)
        .into_iter()
        .flat_map(|view| &view.model.sessions)
        .flat_map(|session| &session.tabs)
        .flat_map(|tab| &tab.panes)
        .map(|pane| pane.id)
        .collect()
}

/// The expectation a dispatched action leaves behind, if it creates something
/// the person will want to be in.
#[must_use]
pub fn expectation(action: ActionId, state: &EngineState, host: &HostId) -> Option<Expectation> {
    match action {
        ActionId::CreateSession | ActionId::CreateTab => Some(Expectation::Tab {
            host: host.clone(),
            known: tabs(state, host),
        }),
        ActionId::CreatePane => Some(Expectation::Pane {
            host: host.clone(),
            known: panes(state, host),
        }),
        _ => None,
    }
}

/// The tab that fulfils a tab expectation, once the model holds it.
#[must_use]
pub fn arrived_tab(state: &EngineState, host: &HostId, known: &BTreeSet<TabId>) -> Option<TabKey> {
    state
        .model()
        .host(host)?
        .model
        .sessions
        .iter()
        .find_map(|session| {
            session
                .tabs
                .iter()
                .find(|tab| !known.contains(&tab.id))
                .map(|tab| TabKey {
                    host: host.clone(),
                    session: session.id,
                    tab: tab.id,
                })
        })
}

/// The pane that fulfils a pane expectation, once the model holds it.
#[must_use]
pub fn arrived_pane(
    state: &EngineState,
    host: &HostId,
    known: &BTreeSet<PaneId>,
) -> Option<PaneId> {
    panes(state, host)
        .into_iter()
        .find(|pane| !known.contains(pane))
}

/// The following a record leaves, or an empty one when there is no file.
#[must_use]
pub fn loaded(path: Option<&std::path::Path>) -> Following {
    let mut following = Following::default();
    let Some(path) = path else {
        return following;
    };
    let record = session_tabs::load(path);
    following.open = record.open;
    following.session_tab = record.shown;
    following
}

/// The hosts added from this window that are connected, have sent their
/// model, and hold no session.
#[must_use]
pub fn ready_to_start<'following>(
    state: &EngineState,
    starting: &'following BTreeSet<HostId>,
) -> Vec<&'following HostId> {
    starting
        .iter()
        .filter(|host| {
            state
                .host(host)
                .is_some_and(|report| matches!(report.connection, HostState::Connected { .. }))
                && state.model().host(host).is_some()
        })
        .collect()
}

impl WindowShell {
    /// Remember the tab now shown, so a later return to its session opens it,
    /// including after this window has quit and a new one has read the record.
    pub(crate) fn remember_shown(&mut self, key: &TabKey) {
        self.following.session_tab.insert(
            SessionKey {
                host: key.host.clone(),
                session: key.session,
            },
            key.tab,
        );
        self.following.open = Some(key.clone());
        if self.following.unwritten_since.is_none() {
            self.following.unwritten_since = Some(std::time::Instant::now());
        }
        self.write_session_tabs(false);
    }

    /// Write the tabs this window has shown, once the record has waited the
    /// selection write delay or when `now` is asked for. A record that cannot
    /// be written leaves the selection as it is: the window still changes tabs.
    pub(crate) fn write_session_tabs(&mut self, now: bool) {
        let Some(since) = self.following.unwritten_since else {
            return;
        };
        if !now && since.elapsed() < self.options.selection_write_delay {
            return;
        }
        self.following.unwritten_since = None;
        let Some(path) = &self.options.selection_path else {
            return;
        };
        let record = SessionTabs {
            open: self.following.open.clone(),
            shown: self.following.session_tab.clone(),
        };
        let _ignored = session_tabs::write(path, &record);
    }

    /// Send a command and, when its effect is already showing, lay the window
    /// out on the neighbor before the host has answered.
    ///
    /// # Errors
    ///
    /// As [`WindowShell::dispatch_command`].
    pub fn dispatch_shown(
        &mut self,
        alias: &str,
        command: SessionCommand,
        window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> Result<Submission, EngineError> {
        let place = self
            .selected()
            .and_then(|selected| bars::tab_place(self.hosts().state(), selected));
        let submission = self.dispatch_command(alias, command)?;
        if submission.optimistic {
            self.reconcile(place.as_ref(), window, context);
            context.notify();
        }
        Ok(submission)
    }

    /// Keep the current tab when the model still holds it. When that tab has
    /// gone, open the one that follows the close, and remember what is shown.
    /// With nothing selected yet, open the tab the record left on screen.
    pub(crate) fn settle_selection(&mut self, place: Option<&TabPlace>) {
        if self
            .selected()
            .is_some_and(|selected| self.tab(selected).is_some())
        {
            return;
        }
        let kept = if place.is_none() {
            self.remembered_open()
        } else {
            None
        };
        if let Some(chosen) = place
            .and_then(|held| bars::tab_after_close(self.hosts().state(), held))
            .or(kept)
            .or_else(|| bars::first_tab(self.hosts().state()))
        {
            self.set_selected(chosen);
        } else {
            self.clear_selected();
        }
    }

    /// The tab the record left on screen, while its session still exists.
    ///
    /// The recorded tab wins while it is still in the session. Otherwise the
    /// session's first remaining tab does, the same way a tab closed while
    /// the window was elsewhere does.
    fn remembered_open(&self) -> Option<TabKey> {
        let open = self.following.open.as_ref()?;
        let key = SessionKey {
            host: open.host.clone(),
            session: open.session,
        };
        let session = self.session(&key)?;
        let tab = if session
            .tabs
            .iter()
            .any(|candidate| candidate.id == open.tab)
        {
            open.tab
        } else {
            let remembered = self.following.session_tab.get(&key).copied();
            bars::shown_tab(self.hosts().state(), &open.host, session, remembered)?
        };
        Some(TabKey {
            host: open.host.clone(),
            session: open.session,
            tab,
        })
    }

    /// Begin holding a host from this window: the stage follows it, and it
    /// gets a first session when it connects holding none.
    ///
    /// # Errors
    ///
    /// Returns the bridge error when the engine has ended.
    pub fn add_host(&mut self, alias: &str) -> Result<(), EngineError> {
        self.hosts_mut().add_host(alias)?;
        let host = HostId(alias.to_owned());
        self.following.preferred = Some(host.clone());
        self.following.starting.insert(host);
        Ok(())
    }

    /// The host a new session goes to: the selected tab's, then the one the
    /// stage describes, then the first held.
    #[must_use]
    pub fn target_host(&self) -> Option<HostId> {
        let state = self.hosts().state();
        self.selected()
            .map(|key| key.host.clone())
            .or_else(|| {
                self.following
                    .preferred
                    .clone()
                    .filter(|host| state.host(host).is_some())
            })
            .or_else(|| state.hosts().next().map(|(host, _)| host.clone()))
    }

    /// Apply a remedy chosen beside a host's state.
    pub fn remedy(&mut self, host: &HostId, remedy: Remedy, context: &mut Context<'_, Self>) {
        let result = match remedy {
            Remedy::Retry => self.hosts_mut().reconnect(&host.0),
            Remedy::Remove | Remedy::Cancel => {
                self.following.starting.remove(host);
                self.hosts_mut().remove_host(&host.0)
            }
        };
        if let Err(error) = result {
            self.failure(host, error.to_string(), context);
        }
        context.notify();
    }

    /// Before a reconcile lays out the selection: start first sessions and
    /// select a tab this window asked for.
    pub(crate) fn follow_model(&mut self, context: &mut Context<'_, Self>) {
        let ready: Vec<HostId> = ready_to_start(self.hosts().state(), &self.following.starting)
            .into_iter()
            .cloned()
            .collect();
        for host in ready {
            self.following.starting.remove(&host);
            let empty = self
                .hosts()
                .state()
                .model()
                .host(&host)
                .is_some_and(|view| view.model.sessions.is_empty());
            if empty && let Err(error) = self.dispatch_action_on(ActionId::CreateSession, &host) {
                self.failure(&host, error.to_string(), context);
            }
        }
        match self.following.expected.clone() {
            Some(Expectation::Tab { host, known }) => {
                if let Some(key) = arrived_tab(self.hosts().state(), &host, &known) {
                    self.following.expected = None;
                    self.set_selected(key);
                }
            }
            Some(Expectation::Pane { host, known }) => {
                if let Some(pane) = arrived_pane(self.hosts().state(), &host, &known) {
                    self.following.expected = None;
                    self.following.focus = Some(PaneKey { host, pane });
                }
            }
            None => {}
        }
    }

    /// After a reconcile created the visible surfaces: give keyboard focus to
    /// the pane a person expects to type into.
    pub(crate) fn follow_focus(&mut self, window: &mut Window, context: &mut Context<'_, Self>) {
        let selected = self.selected().cloned();
        let wanted = self.following.focus.clone().or_else(|| {
            let key = selected.as_ref()?;
            // Staying on this tab keeps the keyboard where it is, including
            // while a palette holds it. A focused pane that has since closed
            // is no longer that place, so the pane still on screen takes it.
            if self.following.focused_tab.as_ref() == Some(key) && !self.focused_pane_is_gone(key) {
                return None;
            }
            let leaves = self.visible_panes();
            let remembered = self
                .hosts()
                .state()
                .model()
                .host(&key.host)
                .and_then(|view| view.focus)
                .filter(|pane| leaves.contains(pane));
            remembered
                .or_else(|| leaves.first().copied())
                .map(|pane| PaneKey {
                    host: key.host.clone(),
                    pane,
                })
        });
        match (wanted, &selected) {
            (Some(key), _) => {
                if let Some(surface) = self.surface(&key) {
                    let handle = surface.read(context).focus_handle(context);
                    window.focus(&handle, context);
                    self.following.focus = None;
                    self.following.focused_tab = selected;
                }
            }
            (None, None) if self.following.focused_tab.is_some() => {
                self.following.focused_tab = None;
                let handle = self.focus_handle(context);
                window.focus(&handle, context);
            }
            (None, _) => {}
        }
    }

    /// Whether the pane last recorded as focused is no longer on screen.
    fn focused_pane_is_gone(&self, key: &TabKey) -> bool {
        let Some(focus) = self
            .hosts()
            .state()
            .model()
            .host(&key.host)
            .and_then(|view| view.focus)
        else {
            return false;
        };
        !self.visible_panes().contains(&focus)
    }
}
