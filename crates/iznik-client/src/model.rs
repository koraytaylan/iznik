//! Everything the client knows, and nothing it does.
//!
//! One host model per host — the server's own, replaced whole or reconciled —
//! and beside it the things the server does not hold: which panes this client
//! subscribes to, the byte each subscription has reached, which pane has the
//! focus, and the commands that have been sent and not yet answered.
//!
//! The cursor is the point of the whole file. A link that dies is a link that
//! comes back, and what makes a pane's stream survive that is knowing, for
//! every subscribed pane, the sequence this client holds — so the resume asks
//! for the next byte rather than for a screen. Nothing here does any I/O; it
//! is a value, and the reducer and the manager are what move it.

use std::collections::BTreeMap;
use std::time::Instant;

use core::fmt::{self, Display, Formatter};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::SessionCommand;
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence};
use iznik_protocol::model::{HostModel, ModelError};

use crate::host::identity::HostId;

/// What a subscription's channel is when it has none of its own just now.
///
/// Channel zero carries the control messages and is never a pane's, so it is
/// free to stand for "the host has not said where this pane's bytes come from"
/// — which is true of a pane whose number was taken by another and whose own
/// announcement has not arrived yet.
pub const NO_CHANNEL: u8 = 0;

/// One subscribed pane: where its bytes arrive, how far they have been read,
/// and how much this client has told the server it may send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscription {
    /// The channel the host announced for it.
    pub channel: u8,
    /// The next byte this client has not seen.
    pub cursor: Sequence,
    /// How many bytes of credit the server has been granted and not spent.
    pub credit_outstanding: u64,
}

impl Subscription {
    /// A subscription that has just been announced, holding nothing yet.
    #[must_use]
    pub fn opened(channel: u8, from: Sequence) -> Subscription {
        Subscription {
            channel,
            cursor: from,
            credit_outstanding: 0,
        }
    }

    /// Moves the cursor on by what arrived.
    ///
    /// Forwards only. The cursor is what a resume asks from, so a count that
    /// would carry it past what a `u64` holds leaves it where it is rather
    /// than wrapping to the beginning of the stream.
    pub fn advance(&mut self, bytes: u64) -> Sequence {
        self.cursor = Sequence(self.cursor.0.saturating_add(bytes));
        self.cursor
    }

    /// Puts the cursor where a screen the server sent begins.
    ///
    /// The one thing that may move it backwards, and only because the server
    /// said so: a client too far behind to be caught up byte by byte is given
    /// a screen and the sequence it stands at.
    pub fn resume_at(&mut self, sequence: Sequence) {
        self.cursor = sequence;
    }

    /// Records credit given to the server.
    pub fn grant(&mut self, bytes: u64) {
        self.credit_outstanding = self.credit_outstanding.saturating_add(bytes);
    }

    /// Records credit the server has spent.
    pub fn spend(&mut self, bytes: u64) {
        self.credit_outstanding = self.credit_outstanding.saturating_sub(bytes);
    }
}

/// A command this client sent and is still showing the effect of.
///
/// It stays here while the host has not answered, and after an answer that
/// applied it while the host has not yet announced the change — so what puts
/// it back if the host refuses it is not a model kept beside it, but the
/// settled model with everything still in flight replayed on top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCommand {
    /// This client's number for it.
    pub id: CommandId,
    /// What was asked.
    pub command: SessionCommand,
    /// The generation the host said it reached, once it has answered.
    ///
    /// A command is answered before its change is announced, so between those
    /// two frames its effect is in what this client shows and in nothing the
    /// host has said yet. It stays here, applied on top like any other, until
    /// a delta or a snapshot brings the model to that generation — and then it
    /// is the host's own and this can let it go.
    pub answered: Option<Generation>,
    /// When it was sent, so it can be given up on.
    pub submitted_at: Instant,
}

/// One host, as this client sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostView {
    /// What this client shows: the settled model with everything still in
    /// flight applied on top of it.
    pub model: HostModel,
    /// The host's own model, as the host last said it stands.
    ///
    /// Kept beside the shown one rather than derived from the commands in
    /// flight, because the host answers a command *before* it announces the
    /// change: between those two frames there is nothing in flight and no
    /// pending entry to hold what the model was, and a delta applied to a
    /// model that already showed its own effect is a delta that cannot be
    /// applied at all.
    pub settled: HostModel,
    /// The panes this client subscribes to, by pane.
    pub subscriptions: BTreeMap<PaneId, Subscription>,
    /// The pane this client is showing, when it is showing one.
    pub focus: Option<PaneId>,
    /// The commands sent and not yet answered, in the order they were sent.
    pub pending: Vec<PendingCommand>,
    /// The last number this client gave a command on this host.
    ///
    /// Kept rather than derived from `pending`, because a number must never be
    /// reused: a command given up on is taken out of `pending`, and an answer
    /// to it that arrives afterwards would otherwise confirm — or roll back —
    /// whichever later command had been given its number.
    pub minted: CommandId,
    /// What the connected server advertised it can decode.
    ///
    /// Empty until a connection's `Hello` says otherwise, so a command nothing
    /// has said this server can decode — the session reorder — is refused
    /// rather than sent to a server that would take the whole connection down
    /// on it.
    pub capabilities: Capabilities,
}

impl Default for HostView {
    fn default() -> HostView {
        HostView::of(HostModel {
            generation: Generation(0),
            sessions: Vec::new(),
        })
    }
}

impl HostView {
    /// A view of a host whose model is `model` and about which nothing else is
    /// known yet.
    #[must_use]
    pub fn of(model: HostModel) -> HostView {
        HostView {
            settled: model.clone(),
            model,
            subscriptions: BTreeMap::new(),
            focus: None,
            pending: Vec::new(),
            minted: CommandId(0),
            capabilities: Capabilities::from_bits(0),
        }
    }

    /// The next number to give a command on this host.
    pub fn mint(&mut self) -> CommandId {
        self.minted = CommandId(self.minted.0.saturating_add(1));
        self.minted
    }

    /// The subscription to `pane`, if this client holds one.
    #[must_use]
    pub fn subscription(&self, pane: PaneId) -> Option<&Subscription> {
        self.subscriptions.get(&pane)
    }

    /// The same, to be moved on.
    pub fn subscription_mut(&mut self, pane: PaneId) -> Option<&mut Subscription> {
        self.subscriptions.get_mut(&pane)
    }

    /// Opens a subscription to `pane` on `channel` from `from`, replacing any
    /// that was there.
    ///
    /// A second announcement for one pane replaces the first: the host has
    /// just said which channel its bytes come on and where they start, and
    /// that is more recent than anything this held.
    ///
    /// It also takes that channel away from any *other* pane holding it. A
    /// host reassigns channel numbers as panes come and go, and after a
    /// reconnection it re-announces every resumed pane on whatever is free —
    /// so a pane not yet re-announced can be left holding a number that now
    /// belongs to another. Bytes arriving on it would then move the wrong
    /// pane's cursor, and the pane whose bytes they were would resume from a
    /// byte it never reached. The host has just said whose channel this is.
    ///
    /// The other pane keeps its subscription, its cursor and the focus if it
    /// had it: what it has lost is a claim that is no longer true, and the
    /// byte it stands at is exactly what its own announcement — or the resume
    /// after the next drop — will need.
    pub fn subscribe(&mut self, pane: PaneId, channel: u8, from: Sequence) -> Subscription {
        for (held, found) in &mut self.subscriptions {
            if *held != pane && found.channel == channel {
                found.channel = NO_CHANNEL;
            }
        }
        let opened = Subscription::opened(channel, from);
        let _replaced = self.subscriptions.insert(pane, opened);
        opened
    }

    /// The channel a pane's bytes arrive on, when it has one.
    ///
    /// A pane whose number another pane has taken has [`NO_CHANNEL`], which is
    /// the control channel and carries nobody's bytes — so it has none, and
    /// anything addressed to it would go where it would not be understood.
    #[must_use]
    pub fn carried(&self, pane: PaneId) -> Option<u8> {
        self.subscriptions
            .get(&pane)
            .map(|held| held.channel)
            .filter(|channel| *channel != NO_CHANNEL)
    }

    /// The pane whose bytes arrive on `channel`, when exactly one does.
    #[must_use]
    pub fn carrying(&self, channel: u8) -> Option<PaneId> {
        if channel == NO_CHANNEL {
            return None;
        }
        let mut found = self
            .subscriptions
            .iter()
            .filter(|(_pane, held)| held.channel == channel)
            .map(|(pane, _held)| *pane);
        let first = found.next()?;
        // Two panes claiming one channel is a state this cannot resolve, and
        // moving the wrong cursor is worse than moving none.
        found.next().is_none().then_some(first)
    }

    /// Drops the subscription to `pane`, and says what it was.
    pub fn unsubscribe(&mut self, pane: PaneId) -> Option<Subscription> {
        let dropped = self.subscriptions.remove(&pane);
        if self.focus == Some(pane) {
            self.focus = None;
        }
        dropped
    }

    /// The pending command with this number.
    #[must_use]
    pub fn awaiting(&self, id: CommandId) -> Option<&PendingCommand> {
        self.pending.iter().find(|held| held.id == id)
    }

    /// Records a command as sent and not yet answered.
    ///
    /// Appended, so the order of `pending` is the order they were sent — which
    /// is the order they must be rolled back in.
    pub fn record(&mut self, pending: PendingCommand) {
        self.pending.push(pending);
    }

    /// Gives up on every command this host answered and never announced.
    ///
    /// What a new connection does with them. The answer said the host reached
    /// a generation; either it is the same host, in which case the snapshot
    /// this arrives with is at that generation or past it and the change is
    /// the host's own, or it is another daemon, which never did it. Both are
    /// reasons to stop showing it, and neither can be told from the other by
    /// the number alone — a daemon that starts again does not always start
    /// below where the last one stopped.
    pub fn forget_answered(&mut self) -> Vec<CommandId> {
        let (kept, gone): (Vec<PendingCommand>, Vec<PendingCommand>) =
            std::mem::take(&mut self.pending)
                .into_iter()
                .partition(|standing| standing.answered.is_none());
        self.pending = kept;
        gone.iter().map(|standing| standing.id).collect()
    }

    /// Every command sent before `moment` and not yet answered, oldest first.
    #[must_use]
    pub fn sent_before(&self, moment: Instant) -> Vec<CommandId> {
        self.pending
            .iter()
            .filter(|held| held.answered.is_none() && held.submitted_at < moment)
            .map(|held| held.id)
            .collect()
    }

    /// Puts what the host has said where this client shows it, and gives back
    /// the model to apply what is still in flight to.
    ///
    /// The caller applies the pending effects afterwards; this is the half
    /// that cannot be done without touching both. What comes back is the
    /// commands this gave up on, which is empty except in the one case below.
    ///
    /// A generation lower than the one already settled is not this host's
    /// history going on — it is another daemon's beginning, after an upgrade
    /// or a restart. A command the daemon that is gone answered can never be
    /// announced now, and nothing else would ever take it out of `pending`:
    /// it is neither expired, since it was answered, nor rolled back, since
    /// it was not refused. It stops being shown, and its number is given back
    /// so the caller can say so. What was sent and never answered stays, and
    /// times out in its own time.
    pub fn settle(&mut self, held: HostModel) -> Vec<CommandId> {
        let restarted = held.generation < self.settled.generation;
        let abandoned = if restarted {
            let (kept, gone): (Vec<PendingCommand>, Vec<PendingCommand>) =
                std::mem::take(&mut self.pending)
                    .into_iter()
                    .partition(|standing| standing.answered.is_none());
            self.pending = kept;
            gone.iter().map(|standing| standing.id).collect()
        } else {
            Vec::new()
        };
        self.settled = held;
        self.model = self.settled.clone();
        abandoned
    }

    /// Whether the model it holds is one the server could have sent.
    ///
    /// # Errors
    ///
    /// The [`ModelError`] of the first invariant it breaks.
    pub fn validate(&self) -> Result<(), ModelError> {
        self.model.validate()
    }
}

/// A host whose model is not one the server could have sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidHost {
    /// Which host.
    pub host: HostId,
    /// What is wrong with its model.
    pub source: ModelError,
}

impl Display for InvalidHost {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let InvalidHost { host, source } = self;
        write!(formatter, "{host}: {source}")
    }
}

impl core::error::Error for InvalidHost {}

/// Everything this client knows, host by host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientModel {
    /// One view per host, ordered by the alias its user gave it.
    pub hosts: BTreeMap<HostId, HostView>,
}

impl ClientModel {
    /// The view of one host.
    #[must_use]
    pub fn host(&self, host: &HostId) -> Option<&HostView> {
        self.hosts.get(host)
    }

    /// The same, to be changed.
    pub fn host_mut(&mut self, host: &HostId) -> Option<&mut HostView> {
        self.hosts.get_mut(host)
    }

    /// Puts a view in, and says what was there.
    ///
    /// Nothing about any other host is touched, which is the property the
    /// whole shape exists for: one host reconnecting, failing or being removed
    /// leaves the others exactly as they were.
    pub fn insert(&mut self, host: HostId, view: HostView) -> Option<HostView> {
        self.hosts.insert(host, view)
    }

    /// Takes a host out, and says what it held.
    pub fn remove(&mut self, host: &HostId) -> Option<HostView> {
        self.hosts.remove(host)
    }

    /// Every host, in the order they are listed.
    #[must_use]
    pub fn aliases(&self) -> Vec<&HostId> {
        self.hosts.keys().collect()
    }

    /// Whether it knows about any host at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// How many panes this client subscribes to, across every host.
    #[must_use]
    pub fn subscribed(&self) -> usize {
        self.hosts
            .values()
            .map(|view| view.subscriptions.len())
            .sum()
    }

    /// Whether every host model it holds is one a server could have sent.
    ///
    /// # Errors
    ///
    /// [`InvalidHost`] naming the first host whose model is not, and what is
    /// wrong with it.
    pub fn validate(&self) -> Result<(), InvalidHost> {
        for (host, view) in &self.hosts {
            view.validate().map_err(|source| InvalidHost {
                host: host.clone(),
                source,
            })?;
        }
        Ok(())
    }
}
