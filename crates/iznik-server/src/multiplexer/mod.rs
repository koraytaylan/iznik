//! One multiplexer per client connection: the pump that carries every
//! subscribed pane over one link under credit windows.
//!
//! One connection carries every pane a client subscribes to, so without
//! arbitration a `cat` of a large file starves the pane the user is typing
//! into. This is therefore a scheduler: the focused pane is served first and
//! with the larger window, the rest round-robin from a moving place, and a
//! cursor at zero credit is skipped rather than waited on.
//!
//! **It buffers no pane bytes of its own.** The history ring is the queue: a
//! subscription is a cursor into it, credit decides how far the cursor
//! advances, and one frame's worth is copied into a buffer the pump owns on
//! its way to the link. A background pane that falls further behind than
//! catching up byte by byte is worth stops being served and is marked stale;
//! when it is looked at again it is sent the truth instead. The ring keeps
//! every byte regardless, so a client that scrolls back after focus can still
//! resume from an older sequence.
//!
//! A `Screen` is serialized before the `PaneChannel` that announces it, and
//! the channel carries the sequence the screen turned out to be exact at
//! rather than the one the plan asked for. The mirror can advance between
//! deciding and serializing, and the contract is that the bytes which follow
//! on the channel begin exactly where the screen ends.

pub mod channel;
pub mod credit;
pub mod scheduler;

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{PaneId, Sequence};
use iznik_protocol::message::{CHANNEL_CONTROL, ToClient, encode_to_client};
use iznik_protocol::model::encode_host_model;
use tokio::sync::{Notify, RwLock, broadcast};
use tokio::task::JoinHandle;

use crate::multiplexer::channel::{ChannelTable, Cursor, MultiplexerError, SinkError};
use crate::pane::{Pane, Subscription};
use crate::resume::{StartPlan, StartRequest, plan_start};
use crate::session::registry::{Numbered, Registry};
use crate::terminal::marks::MarkEvent;

/// What a keystroke's round trip through the multiplexer must stay under at
/// the ninety-ninth percentile while another pane floods.
///
/// Twenty-five milliseconds is the figure a person stops feeling a terminal as
/// immediate past. It is the acceptance for this whole design: a functional
/// test that both panes "work" would pass on an implementation nobody could
/// stand to use.
pub const KEYSTROKE_ROUND_TRIP_BUDGET: Duration = Duration::from_millis(25);

/// Where one frame goes.
///
/// The test sink collects frames in memory; plan 0004 puts a framed link under
/// it. It is a trait rather than a link so that what the scheduler decides can
/// be proven without a socket.
pub trait FrameSink: Send {
    /// Hands one frame to the link: the channel it belongs to and its bytes.
    ///
    /// # Errors
    ///
    /// [`SinkError`] when the link cannot take it.
    fn send(
        &mut self,
        channel: u8,
        payload: &[u8],
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
}

/// One client's view of one host: which panes it watches, on which channels,
/// how far each has been sent, and which one it is looking at.
#[derive(Debug)]
pub struct Multiplexer<Sink: FrameSink> {
    /// The host model and the panes behind it.
    registry: Arc<RwLock<Registry>>,
    /// Where frames go.
    sink: Sink,
    /// Which pane each channel carries.
    channels: ChannelTable,
    /// How far each subscription has been sent, and how much further it may go.
    cursors: BTreeMap<PaneId, Cursor>,
    /// Held for as long as a client watches a pane, so the pane knows not to
    /// answer a program's terminal queries itself.
    subscriptions: BTreeMap<PaneId, Subscription>,
    /// Each subscribed pane's marks.
    marks: BTreeMap<PaneId, broadcast::Receiver<MarkEvent>>,
    /// One task per subscription, forwarding its pane's changes to `wake`,
    /// because a `select!` cannot take a set of futures that changes.
    watchers: BTreeMap<PaneId, JoinHandle<()>>,
    /// Woken when any subscribed pane has more to say.
    wake: Arc<Notify>,
    /// Every change to the model.
    deltas: broadcast::Receiver<Numbered<Delta>>,
    /// Deltas taken off the channel while waiting and not yet sent.
    waiting: VecDeque<Numbered<Delta>>,
    /// Marks taken off their panes' channels and not yet sent, which is the
    /// only copy of them there is.
    owed_marks: VecDeque<(PaneId, MarkEvent)>,
    /// Whether the client must be sent the whole model before anything else.
    owed_snapshot: bool,
    /// The pane this client is looking at.
    focused: Option<PaneId>,
    /// Where the round-robin starts, so the unfocused panes take turns.
    turn: usize,
    /// The one frame's worth of pane bytes the pump owns.
    frame: Vec<u8>,
}

impl<Sink: FrameSink> Drop for Multiplexer<Sink> {
    fn drop(&mut self) {
        for watcher in self.watchers.values() {
            watcher.abort();
        }
    }
}

impl<Sink: FrameSink> Multiplexer<Sink> {
    /// A multiplexer watching nothing yet.
    #[must_use]
    pub fn new(
        registry: Arc<RwLock<Registry>>,
        sink: Sink,
        deltas: broadcast::Receiver<Numbered<Delta>>,
    ) -> Multiplexer<Sink> {
        Multiplexer {
            registry,
            sink,
            channels: ChannelTable::new(),
            cursors: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            marks: BTreeMap::new(),
            watchers: BTreeMap::new(),
            wake: Arc::new(Notify::new()),
            deltas,
            waiting: VecDeque::new(),
            owed_marks: VecDeque::new(),
            owed_snapshot: false,
            focused: None,
            turn: 0,
            frame: Vec::new(),
        }
    }

    /// The pane behind a pane id, without holding the registry across what is
    /// done with it.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::UnknownPane`] when the host holds no such pane.
    async fn pane_of(&self, pane: PaneId) -> Result<Arc<Pane>, MultiplexerError> {
        self.registry
            .read()
            .await
            .pane(pane)
            .cloned()
            .ok_or(MultiplexerError::UnknownPane { pane })
    }

    /// Sends one control message.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::Encoding`] when it will not fit a frame, and
    /// [`MultiplexerError::Sink`] when the link cannot take it.
    async fn tell(&mut self, message: &ToClient) -> Result<(), MultiplexerError> {
        let payload = encode_to_client(message)?;
        self.sink.send(CHANNEL_CONTROL, &payload).await?;
        Ok(())
    }

    /// Sends the whole model, which is what a client gets instead of deltas it
    /// has missed.
    ///
    /// # Errors
    ///
    /// As [`Multiplexer::tell`].
    async fn tell_the_model(&mut self) -> Result<(), MultiplexerError> {
        let model = self.registry.read().await.snapshot();
        let payload = encode_host_model(&model)?;
        self.tell(&ToClient::Snapshot {
            generation: model.generation,
            payload,
        })
        .await
    }
}

impl<Sink: FrameSink> Multiplexer<Sink> {
    /// Begins delivering a pane's output, starting with whatever
    /// [`plan_start`] says: bytes from where the client already is, or the
    /// truth and then bytes from there.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::UnknownPane`] when the host holds no such pane,
    /// [`MultiplexerError::ChannelsExhausted`] when no channel is free,
    /// [`MultiplexerError::Encoding`] when a message will not fit a frame, and
    /// [`MultiplexerError::Sink`] when the link cannot take one.
    pub async fn subscribe(&mut self, request: StartRequest) -> Result<(), MultiplexerError> {
        let pane = request.pane();
        // A screen request arrives on a subscription that already stands, and
        // a refusal must not tear that down; a refusal on a fresh one must not
        // leave the channel consumed, the pane's mirror pinned in subscriber
        // mode and a watcher waking a pump that has no cursor to serve.
        let standing = self.cursors.contains_key(&pane);
        match self.begin(request, pane).await {
            Ok(()) => Ok(()),
            Err(refused) => {
                if !standing {
                    self.forget(pane);
                }
                Err(refused)
            }
        }
    }

    /// Everything [`Multiplexer::subscribe`] does, so that it can unwind.
    ///
    /// # Errors
    ///
    /// As [`Multiplexer::subscribe`].
    async fn begin(&mut self, request: StartRequest, pane: PaneId) -> Result<(), MultiplexerError> {
        let held = self.pane_of(pane).await?;
        let state = held.state();
        let channel = self.channels.assign(pane)?;
        let plan = plan_start(&request, state.oldest, state.newest);
        // A `ScreenRequest` arrives on a subscription that already has a
        // window, and the client's outstanding credit is a property of the
        // channel rather than of the request. Carrying it over is what keeps a
        // screen request from handing out bytes the client never granted.
        let granted = self.cursors.get(&pane).map(|cursor| cursor.credit);

        // Held before the first byte is read, so the pane stops answering a
        // program's terminal queries itself: this client's emulator answers.
        self.subscriptions.insert(pane, held.subscribe());
        self.marks.insert(pane, held.marks());
        let mut changes = held.state_updates();
        let wake = Arc::clone(&self.wake);
        let watching = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                wake.notify_one();
            }
        });
        // Dropping a `JoinHandle` detaches its task rather than ending it, and
        // a screen request on a live subscription lands here again, so the one
        // this replaces is ended rather than left waking the pump for ever.
        if let Some(replaced) = self.watchers.insert(pane, watching) {
            replaced.abort();
        }

        let sequence = match plan {
            StartPlan::Continue { from } => {
                self.tell(&ToClient::PaneChannel {
                    pane,
                    channel,
                    sequence: from,
                })
                .await?;
                from
            }
            StartPlan::Screen { at: _asked } => {
                // Serialized first: the mirror can advance between deciding
                // and serializing, and what the channel announces has to be
                // where the bytes that follow actually begin.
                let screen = held.screen().await.map_err(|error| {
                    MultiplexerError::Sink(SinkError::Refused {
                        detail: error.to_string(),
                    })
                })?;
                self.tell(&ToClient::PaneChannel {
                    pane,
                    channel,
                    sequence: screen.sequence,
                })
                .await?;
                self.tell(&ToClient::Screen {
                    pane,
                    sequence: screen.sequence,
                    columns: screen.columns,
                    rows: screen.rows,
                    bytes: screen.bytes,
                })
                .await?;
                screen.sequence
            }
        };
        let mut cursor = Cursor::new(pane, channel, sequence);
        match granted {
            Some(window) => cursor.credit = window,
            None => {
                if self.focused == Some(pane) {
                    cursor.credit.widen();
                }
            }
        }
        self.cursors.insert(pane, cursor);
        Ok(())
    }

    /// Stops delivering a pane's output and holds its channel back until the
    /// client says nothing of it is still on the wire.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::NotSubscribed`] when the client does not watch it,
    /// and the encoding and link refusals of [`MultiplexerError::Encoding`]
    /// and [`MultiplexerError::Sink`].
    pub async fn unsubscribe(&mut self, pane: PaneId) -> Result<(), MultiplexerError> {
        let Some(channel) = self.channels.channel_of(pane) else {
            return Err(MultiplexerError::NotSubscribed { pane });
        };
        self.tell(&ToClient::PaneDetached { pane, channel }).await?;
        self.forget(pane);
        Ok(())
    }

    /// Forgets everything about a subscription but the channel, which waits.
    fn forget(&mut self, pane: PaneId) {
        if let Some(channel) = self.channels.channel_of(pane) {
            self.channels.release(channel);
        }
        let _cursor = self.cursors.remove(&pane);
        let _subscription = self.subscriptions.remove(&pane);
        let _marks = self.marks.remove(&pane);
        if let Some(watcher) = self.watchers.remove(&pane) {
            watcher.abort();
        }
        if self.focused == Some(pane) {
            self.focused = None;
        }
    }

    /// Takes the client's word that nothing of a detached pane is still on the
    /// wire, so the channel number can be used again.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::NotReleased`] when it was not waiting.
    pub fn channel_released(&mut self, channel: u8) -> Result<(), MultiplexerError> {
        self.channels.acknowledge(channel)
    }

    /// Returns flow-control credit as the client consumes it.
    ///
    /// Credit for a channel that carries nothing is dropped rather than
    /// refused: the server detaches a pane and the client's `Credit` frames
    /// for it are already in flight, which is an ordinary race and not a
    /// client acting on a pane it never had.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::NotSubscribed`] when the channel carries a pane
    /// this client has no cursor for.
    pub fn credit(&mut self, channel: u8, bytes: u32) -> Result<(), MultiplexerError> {
        let Some(pane) = self.channels.pane_of(channel) else {
            return Ok(());
        };
        let Some(cursor) = self.cursors.get_mut(&pane) else {
            return Err(MultiplexerError::NotSubscribed { pane });
        };
        cursor.credit.refill(bytes);
        // A cursor skipped at zero credit is not woken by its pane, which may
        // be at a prompt saying nothing; without this its backlog waits for
        // output that never comes.
        self.wake.notify_one();
        Ok(())
    }

    /// Says which pane the client is looking at: it is served first, with the
    /// larger window, and its history is the last the budget takes from.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::NotSubscribed`] when the client does not watch it.
    pub async fn focus(&mut self, pane: PaneId) -> Result<(), MultiplexerError> {
        if !self.cursors.contains_key(&pane) {
            return Err(MultiplexerError::NotSubscribed { pane });
        }
        self.registry.read().await.touch(pane);
        // Widening twice would add the increment twice, and a client that
        // says what it is already looking at — after a reconnect, or on every
        // window activation — would push the server past the ceiling the
        // window exists to hold it to.
        if self.focused == Some(pane) {
            return Ok(());
        }
        if let Some(left) = self.focused
            && let Some(cursor) = self.cursors.get_mut(&left)
        {
            cursor.credit.narrow();
        }
        self.focused = Some(pane);
        if let Some(cursor) = self.cursors.get_mut(&pane) {
            cursor.credit.widen();
        }
        // The catch-up a stale cursor is owed on focus is exactly what a
        // parked pump is waiting to be told about, and an idle pane will not
        // tell it.
        self.wake.notify_one();
        Ok(())
    }

    /// Which panes the client watches.
    #[must_use]
    pub fn subscribed(&self) -> Vec<PaneId> {
        self.cursors.keys().copied().collect()
    }
}

impl<Sink: FrameSink> Multiplexer<Sink> {
    /// Carries everything that is ready: the model's changes, the marks of the
    /// panes this client watches, and one scheduling round of pane bytes. Says
    /// whether it sent anything, so a caller knows whether to go round again
    /// or wait.
    ///
    /// # Errors
    ///
    /// [`MultiplexerError::Encoding`] and [`MultiplexerError::Sink`] when a
    /// message cannot be sent, and [`MultiplexerError::UnknownPane`] when a
    /// pane goes while it is served.
    pub async fn pump(&mut self) -> Result<bool, MultiplexerError> {
        let mut sent = self.carry_deltas().await?;
        sent |= self.carry_marks().await?;
        sent |= self.round().await?;
        Ok(sent)
    }

    /// Waits until there is something to pump, so a client with nothing
    /// happening and a client whose cursors have no credit both cost nothing.
    pub async fn ready(&mut self) {
        if !self.waiting.is_empty() || self.owed_snapshot {
            return;
        }
        tokio::select! {
            received = self.deltas.recv() => match received {
                Ok(numbered) => self.waiting.push_back(numbered),
                Err(broadcast::error::RecvError::Lagged(_missed)) => self.owed_snapshot = true,
                Err(broadcast::error::RecvError::Closed) => {}
            },
            () = self.wake.notified() => {}
        }
    }

    /// Sends every change to the model in order, or the whole model when the
    /// client has fallen further behind than the channel holds — which is what
    /// its reconciler would have asked for anyway.
    ///
    /// # Errors
    ///
    /// The refusals of [`Multiplexer::tell`].
    async fn carry_deltas(&mut self) -> Result<bool, MultiplexerError> {
        let mut sent = false;
        loop {
            let numbered = match self.waiting.pop_front() {
                Some(numbered) => numbered,
                None => match self.deltas.try_recv() {
                    Ok(numbered) => numbered,
                    Err(broadcast::error::TryRecvError::Lagged(_missed)) => {
                        self.owed_snapshot = true;
                        continue;
                    }
                    Err(_gone) => break,
                },
            };
            if self.owed_snapshot {
                // A snapshot is later than anything still waiting, so what was
                // waiting is dropped rather than applied on top of it.
                continue;
            }
            let payload = match encode_delta(&numbered.value) {
                Ok(payload) => payload,
                Err(refused) => {
                    // A change this client cannot be told is a client that
                    // needs the whole model, not one that never hears of it.
                    tracing::warn!(
                        generation = numbered.generation.0,
                        ?refused,
                        "a delta would not encode"
                    );
                    self.owed_snapshot = true;
                    continue;
                }
            };
            let told = self
                .tell(&ToClient::Delta {
                    generation: numbered.generation,
                    payload,
                })
                .await;
            if let Err(refused) = told {
                // Dropping it here would leave the client a generation behind
                // for ever, with nothing owed that would repair it.
                self.waiting.push_front(numbered);
                return Err(refused);
            }
            sent = true;
        }
        if self.owed_snapshot {
            self.waiting.clear();
            self.tell_the_model().await?;
            self.owed_snapshot = false;
            sent = true;
        }
        Ok(sent)
    }

    /// Sends every shell-integration mark of every pane this client watches.
    ///
    /// # Errors
    ///
    /// The refusals of [`Multiplexer::tell`].
    async fn carry_marks(&mut self) -> Result<bool, MultiplexerError> {
        let mut carried = Vec::new();
        for (pane, marks) in &mut self.marks {
            loop {
                match marks.try_recv() {
                    Ok(event) => carried.push((*pane, event)),
                    // A client that fell behind on marks is not out of step:
                    // the ring is the durable record of what the pane said.
                    Err(broadcast::error::TryRecvError::Lagged(_missed)) => {}
                    Err(_gone) => break,
                }
            }
        }
        self.owed_marks.extend(carried);
        let sent = !self.owed_marks.is_empty();
        while let Some((pane, event)) = self.owed_marks.pop_front() {
            let told = self
                .tell(&ToClient::Mark {
                    pane,
                    sequence: event.sequence,
                    kind: event.kind.clone(),
                })
                .await;
            if let Err(refused) = told {
                // Taken off the receiver and not yet sent: the only copy.
                self.owed_marks.push_front((pane, event));
                return Err(refused);
            }
        }
        Ok(sent)
    }

    /// One scheduling round: the focused cursor first, then the rest from
    /// where the last round left off.
    ///
    /// # Errors
    ///
    /// The refusals of [`Multiplexer::tell`], and
    /// [`MultiplexerError::UnknownPane`] when a pane goes while it is served.
    async fn round(&mut self) -> Result<bool, MultiplexerError> {
        let watched = self.subscribed();
        let order = scheduler::order(&watched, self.focused, self.turn);
        self.turn = self.turn.wrapping_add(1);
        let mut sent = false;
        for pane in order {
            sent |= self.serve(pane).await?;
        }
        Ok(sent)
    }

    /// Serves one cursor: catches it up with the truth if it is stale and can
    /// be, marks it stale if it has fallen too far, and otherwise sends it up
    /// to one frame of what it has not seen.
    ///
    /// # Errors
    ///
    /// The refusals of [`Multiplexer::tell`], and
    /// [`MultiplexerError::UnknownPane`] when the pane has gone.
    async fn serve(&mut self, pane: PaneId) -> Result<bool, MultiplexerError> {
        let Some(cursor) = self.cursors.get(&pane).copied() else {
            return Ok(false);
        };
        let Ok(held) = self.pane_of(pane).await else {
            // The pane went while this client watched it; the registry has
            // said so in a delta and the client is told the channel is done.
            self.unsubscribe(pane).await?;
            return Ok(true);
        };
        let state = held.state();
        let focused = self.focused == Some(pane);

        if cursor.stale {
            if focused || cursor.credit.available() > 0 {
                return self.catch_up(pane, &held).await.map(|()| true);
            }
            return Ok(false);
        }
        if scheduler::has_fallen_behind(&cursor, state.newest, focused) {
            if let Some(marked) = self.cursors.get_mut(&pane) {
                marked.stale = true;
            }
            return Ok(false);
        }
        let allowance = scheduler::allowance(&cursor);
        if allowance == 0 || cursor.sequence >= state.newest {
            return Ok(false);
        }
        self.frame.clear();
        if held
            .copy_history(cursor.sequence, allowance, &mut self.frame)
            .is_err()
        {
            // The ring aged past the cursor between deciding and reading, so
            // the truth is what it needs rather than the bytes it asked for.
            return self.catch_up(pane, &held).await.map(|()| true);
        }
        if self.frame.is_empty() {
            return Ok(false);
        }
        let bytes = std::mem::take(&mut self.frame);
        self.sink.send(cursor.channel, &bytes).await?;
        self.frame = bytes;
        let count = self.frame.len();
        if let Some(advanced) = self.cursors.get_mut(&pane) {
            let carried = u64::try_from(count).unwrap_or(0);
            advanced.sequence = Sequence(advanced.sequence.0.saturating_add(carried));
            let _taken = advanced
                .credit
                .consume(u32::try_from(count).unwrap_or(u32::MAX));
        }
        Ok(true)
    }

    /// Sends a stale or aged-out cursor the truth and puts it at the sequence
    /// the truth is exact at, so the bytes that follow begin exactly there.
    ///
    /// # Errors
    ///
    /// The refusals of [`Multiplexer::tell`].
    async fn catch_up(&mut self, pane: PaneId, held: &Arc<Pane>) -> Result<(), MultiplexerError> {
        let Some(channel) = self.channels.channel_of(pane) else {
            return Err(MultiplexerError::NotSubscribed { pane });
        };
        let screen = held.screen().await.map_err(|error| {
            MultiplexerError::Sink(SinkError::Refused {
                detail: error.to_string(),
            })
        })?;
        self.tell(&ToClient::PaneChannel {
            pane,
            channel,
            sequence: screen.sequence,
        })
        .await?;
        self.tell(&ToClient::Screen {
            pane,
            sequence: screen.sequence,
            columns: screen.columns,
            rows: screen.rows,
            bytes: screen.bytes,
        })
        .await?;
        if let Some(cursor) = self.cursors.get_mut(&pane) {
            cursor.sequence = screen.sequence;
            cursor.stale = false;
        }
        Ok(())
    }
}
