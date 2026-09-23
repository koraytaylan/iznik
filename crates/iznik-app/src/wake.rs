//! The signal an owner thread raises after queueing something for the window.
//!
//! The engine bridge and the emulator thread each put their results on a
//! channel the window drains. Rather than the window looking at those channels
//! on a timer, each owner raises its signal after every send, and the window's
//! pump awaits it. A signal raised while nobody is waiting is kept, so an event
//! queued while the window is busy draining still wakes the next wait.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;

use tokio::sync::Notify;

/// One owner's wakeup, cloned into every thread that queues for the window.
#[derive(Clone, Debug, Default)]
pub struct WakeSignal {
    /// Stores one permit when raised with no waiter, so no wakeup is lost.
    notify: Arc<Notify>,
}

impl WakeSignal {
    /// A signal nobody has raised yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Wake the waiting pump, or the next one to wait.
    pub fn raise(&self) {
        self.notify.notify_one();
    }

    /// Resolves once the signal has been raised since the previous wait ended.
    pub async fn raised(&self) {
        self.notify.notified().await;
    }
}

/// Resolve when either signal is raised or `fallback` completes, whichever
/// happens first. The fallback is the pump's slow timer, which keeps the
/// settings poll and any missed wakeup from stalling forever.
pub(crate) async fn either(first: &WakeSignal, second: &WakeSignal, fallback: impl Future) {
    let mut first = pin!(first.raised());
    let mut second = pin!(second.raised());
    let mut fallback = pin!(fallback);
    poll_fn(|context| {
        if first.as_mut().poll(context).is_ready()
            || second.as_mut().poll(context).is_ready()
            || fallback.as_mut().poll(context).is_ready()
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}

/// Give the executor one turn before continuing, so a pump with more queued
/// work lets a frame be drawn between its drains.
pub(crate) async fn pass_turn() {
    let mut yielded = false;
    poll_fn(|context| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}
