//! What the application shows about the engine: one host's life, the model as
//! this window sees it, and everything worth telling a person.
//!
//! There are two types here and the split is the point. [`EngineState`] is the
//! whole of what the window knows, as a value: per-host connection state, the
//! host's model, the upgrades on offer, and the notices. It names no engine and
//! starts no thread, so what it does with an event is a pure function of that
//! event and can be established — including over a thousand generated model
//! sequences — without a daemon anywhere. [`HostUi`] is that value with the
//! engine beside it, and is what the chrome is written against: it drains the
//! engine's channel into the state and forwards the calls that return at once.
//!
//! The model is kept by the client's own reducer rather than by a second
//! implementation of it. A snapshot or a delta arrives as the bytes the wire
//! carried, [`EngineState`] turns them back into the message they were, and
//! `iznik_client::reduce::reduce` applies it. So the mirror converges where the
//! engine converges by construction and not by agreement: there is one
//! implementation of applying a change, and a window that drifted from the
//! engine would be a bug in that one.
//!
//! Nothing here calls the engine on a path that waits on it. The three
//! operations whose manager calls wait for a host's task — removing a host,
//! upgrading one, taking iznik off one — return before they are done, and what
//! they answered arrives as a notice. See `bridge.rs` for why.
//!
//! # What an offer is, and what an error is
//!
//! A host that connects running a newer server than this build carries returns
//! an [`UpgradeOffer`] with its connection. That is *state*: [`HostReport`]
//! holds it and [`EngineState::upgrade_offer`] hands it to a dialog, which
//! renders it as a question with two answers. It is deliberately not a
//! [`NoticeKind::Failure`] and not a [`NoticeKind::Refusal`]: nothing has
//! failed, nothing was refused, and offering a person a choice is not the same
//! act as telling them something broke.

use std::collections::BTreeMap;

use iznik_client::commands::Submission;
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::{ManagerError, ManagerEvent};
use iznik_client::host::state::{HostState, UpgradeOffer};
use iznik_client::model::ClientModel;
use iznik_client::reduce::{Effect, Notification, reduce};
use iznik_protocol::command::{CommandOutcome, SessionCommand};
use iznik_protocol::identity::CommandId;
use iznik_protocol::message::ToClient;

use crate::bridge::{EngineBridge, EngineError, EngineEvent, Operation};

/// Where one host is in its life, and what its connection brought with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostReport {
    /// The state the manager last said this host was in.
    pub connection: HostState,
    /// A newer server the host is running than this build carries, when it
    /// returned one with its connection.
    ///
    /// Held until the person answers the dialog, and cleared by a connection
    /// that carries no offer of its own — so a host upgraded behind this
    /// window's back stops offering rather than offering for ever.
    pub upgrade: Option<UpgradeOffer>,
}

impl HostReport {
    /// A host nothing has been said about yet.
    #[must_use]
    pub fn unknown() -> HostReport {
        HostReport {
            connection: HostState::Disconnected,
            upgrade: None,
        }
    }
}

/// What a notice is about, so that a surface can style, sort and dismiss it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NoticeKind {
    /// A host moved: probing, connecting, connected, reconnecting, failed.
    Connection,
    /// A newer server is on offer. State, not a failure and not a refusal.
    Offer,
    /// A host refused something this client asked for, and said why.
    Refusal,
    /// Something failed: a command nobody answered, a host that could not be
    /// reached, an operation that would not be performed, or something the host
    /// said that could not be read.
    Failure,
    /// A command this client sent was applied.
    Command,
    /// A shell-integration event in a pane.
    Mark,
}

/// One thing worth telling a person, about one host.
///
/// Every notice names the host it is about, because a window holding several
/// hosts must say which one it means. The words are the host's own, the
/// manager's, or the protocol's own rendering of a code — never this
/// application's guess at what they were trying to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The host it is about.
    pub host: HostId,
    /// What it is about.
    pub kind: NoticeKind,
    /// What to show.
    pub detail: String,
}

/// Everything the window knows, as a value with no engine in it.
#[derive(Clone, Debug, Default)]
pub struct EngineState {
    /// The hosts' models, as the engine's own reducer arrives at them.
    model: ClientModel,
    /// Where each host the engine has said anything about is in its life.
    hosts: BTreeMap<HostId, HostReport>,
    /// What has been said and not yet taken by a surface.
    notices: Vec<Notice>,
}

impl EngineState {
    /// A window that has heard nothing yet.
    #[must_use]
    pub fn new() -> EngineState {
        EngineState::default()
    }

    /// The models of every host, as the host itself has said they are.
    #[must_use]
    pub fn model(&self) -> &ClientModel {
        &self.model
    }

    /// Where one host is in its life, once the engine has said anything about
    /// it.
    #[must_use]
    pub fn host(&self, host: &HostId) -> Option<&HostReport> {
        self.hosts.get(host)
    }

    /// Every host the engine has said anything about, in alias order.
    pub fn hosts(&self) -> impl Iterator<Item = (&HostId, &HostReport)> {
        self.hosts.iter()
    }

    /// The newer server a host is offering, when it is offering one.
    ///
    /// What a dialog renders, and the whole of the condition for showing it: a
    /// connection that carried an offer is what put it here.
    #[must_use]
    pub fn upgrade_offer(&self, host: &HostId) -> Option<&UpgradeOffer> {
        self.hosts.get(host)?.upgrade.as_ref()
    }

    /// Whether a connected host's server advertised it can reorder sessions.
    ///
    /// False for a host nothing has been said about and for one that is not
    /// connected, which is what a surface needs: a server of a build that
    /// predates `ReorderSessions` refuses the command as garbage, so the
    /// entries that would send it are offered only to a host whose connection
    /// carried the capability.
    #[must_use]
    pub fn reorders_sessions(&self, host: &HostId) -> bool {
        self.hosts
            .get(host)
            .is_some_and(|report| report.connection.reorders_sessions())
    }

    /// Whether a held host's connected server is still missing capabilities
    /// this build knows.
    ///
    /// False for a host nothing is known about and for one whose server has
    /// every bit, so it is exactly the condition a once-per-host notice is
    /// kept against.
    #[must_use]
    pub fn missing_capabilities_for(&self, host: &HostId) -> bool {
        self.hosts
            .get(host)
            .is_some_and(|report| report.connection.missing_capabilities().bits() != 0)
    }

    /// Everything said since the last call, in the order it was said, leaving
    /// nothing behind.
    ///
    /// Taken rather than read: a notice a surface has shown is not one the
    /// window must hold for ever, and the engine says something on every
    /// connection and every mark.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    /// Applies one thing the engine said.
    ///
    /// The whole of how state gets in, and the only place a `ManagerEvent` is
    /// read.
    pub fn absorb(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Said(ManagerEvent::Moved { host, state }) => self.moved(&host, &state),
            EngineEvent::Said(ManagerEvent::Snapshot {
                host,
                generation,
                payload,
            }) => self.apply(
                &host,
                &ToClient::Snapshot {
                    generation,
                    payload,
                },
            ),
            EngineEvent::Said(ManagerEvent::Delta {
                host,
                generation,
                payload,
            }) => self.apply(
                &host,
                &ToClient::Delta {
                    generation,
                    payload,
                },
            ),
            EngineEvent::Said(ManagerEvent::Notify(notification)) => self.recorded(&notification),
            EngineEvent::Said(ManagerEvent::Removed { host }) => self.removed(host),
            // A pane's bytes, its screen and its detaching are the terminal's
            // business and not the window's: the grid element is fed on the
            // engine's own pane path, and nothing here draws a byte.
            EngineEvent::Said(
                ManagerEvent::Screen { .. }
                | ManagerEvent::Bytes { .. }
                | ManagerEvent::Detached { .. },
            ) => {}
            EngineEvent::Finished { operation, answer } => self.finished(&operation, answer),
        }
    }

    /// Applies one message from a host to that host's model.
    ///
    /// Public because a case about convergence is a case about exactly this:
    /// the same messages, applied the same way, with no engine and no thread in
    /// the way.
    pub fn apply(&mut self, host: &HostId, message: &ToClient) {
        for effect in reduce(&mut self.model, host, message) {
            match effect {
                Effect::Notify(Notification::Malformed { detail, .. }) => {
                    self.notice(host.clone(), NoticeKind::Failure, detail);
                }
                Effect::Notify(other) => self.recorded(&other),
                // The engine has already acted on these before it passed the
                // message on: a gap asks the host for the whole of its model
                // and never reaches this window, and a screen is the terminal's.
                Effect::RequestSnapshot
                | Effect::ReleaseChannel { .. }
                | Effect::Screen { .. }
                | Effect::Abandoned { .. } => {}
            }
        }
    }

    /// One host moved, and this is where it went.
    fn moved(&mut self, host: &HostId, state: &HostState) {
        let offer = match state {
            HostState::Connected { upgrade, .. } => upgrade.clone(),
            _otherwise => None,
        };
        let standing = self
            .hosts
            .entry(host.clone())
            .or_insert_with(HostReport::unknown);
        standing.connection.clone_from(state);
        // Replaced rather than kept when a connection says there is none: a
        // host that has been upgraded since is not one still offering.
        if matches!(state, HostState::Connected { .. }) {
            standing.upgrade.clone_from(&offer);
        }
        let kind = if matches!(state, HostState::Failed { .. }) {
            NoticeKind::Failure
        } else {
            NoticeKind::Connection
        };
        self.notice(host.clone(), kind, state.to_string());
        if let Some(offered) = offer {
            self.notice(
                host.clone(),
                NoticeKind::Offer,
                offered_summary(host, &offered),
            );
        }
    }

    /// A host is no longer held at all.
    fn removed(&mut self, host: HostId) {
        let _gone = self.hosts.remove(&host);
        let _dropped = self.model.remove(&host);
        self.notice(host, NoticeKind::Connection, "no longer held".to_owned());
    }

    /// An operation performed off the drawing thread has finished.
    ///
    /// A refusal is said aloud, in the manager's own words, because the person
    /// who asked for the operation is looking at a window that has otherwise
    /// said nothing: an upgrade that would have ended live panes, a host that
    /// could not be reached, a taking-off that did not finish. One that worked
    /// says nothing here — what became of the host arrives as the state it
    /// moved to.
    fn finished(&mut self, operation: &Operation, answer: Result<(), ManagerError>) {
        match answer {
            Ok(()) => {}
            Err(refusal) => self.notice(
                operation.host().clone(),
                NoticeKind::Failure,
                format!("{operation}: {refusal}"),
            ),
        }
    }

    /// Something worth telling a person, in the words whatever said it used.
    fn recorded(&mut self, notification: &Notification) {
        let (host, kind, detail) = read(notification);
        self.notice(host, kind, detail);
    }

    /// Records one notice.
    fn notice(&mut self, host: HostId, kind: NoticeKind, detail: String) {
        self.notices.push(Notice { host, kind, detail });
    }
}

/// The application's engine and everything it has said, as one entity.
///
/// Owned by the window's thread and by nothing else. The engine's channel is a
/// single-consumer one, so there is one of these per window and no lock in
/// front of any of it.
#[derive(Debug)]
pub struct HostUi {
    /// Everything the window knows.
    state: EngineState,
    /// The engine itself, which only this entity holds.
    bridge: EngineBridge,
}

impl HostUi {
    /// The application's engine view, over an engine already started.
    #[must_use]
    pub fn new(bridge: EngineBridge) -> HostUi {
        HostUi {
            state: EngineState::new(),
            bridge,
        }
    }

    /// Takes everything the engine has said since the last call and applies it.
    ///
    /// The one call the window makes in each update cycle, and the only way
    /// state gets in. It never waits: an engine that has said nothing does
    /// nothing, and the drain returns as soon as the channel is empty.
    pub fn absorb(&mut self) {
        for event in self.bridge.drain() {
            self.absorb_event(event);
        }
    }

    /// The nonblocking engine facade used by pane lifecycle and rendering routes.
    #[must_use]
    pub fn bridge(&self) -> &EngineBridge {
        &self.bridge
    }

    /// Apply an event after the shell has routed any terminal payload it carries.
    pub fn absorb_event(&mut self, event: EngineEvent) {
        self.state.absorb(event);
    }

    /// Everything the window knows, to read.
    #[must_use]
    pub fn state(&self) -> &EngineState {
        &self.state
    }

    /// Everything said since the last call, leaving nothing behind.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        self.state.take_notices()
    }

    /// Begins holding a host, and connecting to it.
    ///
    /// Returns at once: the connecting is the host's own task's business, and
    /// what becomes of it arrives as a state.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn add_host(&mut self, alias: &str) -> Result<(), EngineError> {
        self.bridge.add_host(alias)
    }

    /// Stops holding a host, and forgets what this window knew about it.
    ///
    /// Returns before the host is gone: what the manager answered arrives as a
    /// notice.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn remove_host(&mut self, alias: &str) -> Result<(), EngineError> {
        self.bridge.remove_host(alias)
    }

    /// Drops a host's link and opens another at once, without waiting out its
    /// backoff.
    ///
    /// # Errors
    ///
    /// [`EngineError::Manager`] with the manager's refusal — a host it does not
    /// hold, or one whose task has already ended — and [`EngineError::Stopped`]
    /// when the engine has ended.
    pub fn reconnect(&mut self, alias: &str) -> Result<(), EngineError> {
        self.bridge.reconnect(alias)
    }

    /// Replaces the server on a host with the one this build carries, ending
    /// the panes it holds when `force` says to.
    ///
    /// Returns before the host has been reached: what the manager answered
    /// arrives as a notice.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn upgrade(&mut self, alias: &str, force: bool) -> Result<(), EngineError> {
        self.bridge.upgrade(alias, force)
    }

    /// Takes iznik off a host.
    ///
    /// Returns before the host has been reached: what the manager answered
    /// arrives as a notice.
    ///
    /// # Errors
    ///
    /// [`EngineError::Stopped`] when the engine has ended.
    pub fn uninstall(&mut self, alias: &str) -> Result<(), EngineError> {
        self.bridge.uninstall(alias)
    }

    /// Sends a session command over the engine's optimistic path.
    ///
    /// # Errors
    ///
    /// [`EngineError::Manager`] with the manager's refusal — a host it does not
    /// hold, or one whose task has ended — and [`EngineError::Stopped`] when
    /// the engine has ended.
    pub fn command(
        &mut self,
        alias: &str,
        command: SessionCommand,
    ) -> Result<Submission, EngineError> {
        self.bridge.command(alias, command)
    }
}

/// One notification as what it is about and what to show.
fn read(notification: &Notification) -> (HostId, NoticeKind, String) {
    match notification {
        Notification::CommandFinished {
            host,
            command,
            outcome,
        } => (host.clone(), kind_of(outcome), answered(*command, outcome)),
        Notification::CommandTimedOut { host, command } => (
            host.clone(),
            NoticeKind::Failure,
            format!("command {} was never answered", command.0),
        ),
        Notification::Mark { host, pane, .. } => (
            host.clone(),
            NoticeKind::Mark,
            format!("pane {} reported something", pane.0),
        ),
        Notification::Refused {
            host,
            code,
            message,
        } => (
            host.clone(),
            NoticeKind::Refusal,
            format!("{code:?}: {message}"),
        ),
        Notification::Malformed { host, detail } => {
            (host.clone(), NoticeKind::Failure, detail.clone())
        }
    }
}

/// What kind of notice an answer to a command is.
fn kind_of(outcome: &CommandOutcome) -> NoticeKind {
    match outcome {
        CommandOutcome::Applied { .. } => NoticeKind::Command,
        CommandOutcome::Rejected { .. } => NoticeKind::Refusal,
    }
}

/// The words for a command's answer, naming the command the host is answering
/// so that whoever sent it knows which of theirs this is.
fn answered(command: CommandId, outcome: &CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Applied { .. } => format!("command {} was applied", command.0),
        CommandOutcome::Rejected { code, message } => {
            format!("command {} was refused: {code:?}: {message}", command.0)
        }
    }
}

/// One sentence saying which server is on offer, which the host is running,
/// and why the offer is being made.
fn offered_summary(host: &HostId, offer: &UpgradeOffer) -> String {
    format!(
        "{}: the host runs iznik {}, this build carries iznik {}",
        offer.summary(host),
        offer.installed.crate_version,
        offer.bundled.crate_version
    )
}
