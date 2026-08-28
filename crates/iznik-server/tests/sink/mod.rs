//! The in-memory sink the multiplexer's frames go to, and the frame it sends.
//!
//! A module of its own because the cases that use it are a file of their own
//! and the two together do not fit under this repository's thousand-line
//! ceiling — which is what the ceiling is for. Plan 0004 puts a framed link
//! where this stands.

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use iznik_protocol::message::CHANNEL_CONTROL;
use iznik_server::multiplexer::FrameSink;
use iznik_server::multiplexer::channel::SinkError;

/// A frame the multiplexer sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The channel it went out on.
    pub(crate) channel: u8,
    /// Its bytes.
    pub(crate) payload: Vec<u8>,
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
pub(crate) struct Collected {
    /// What it has been handed.
    inner: Arc<Mutex<Sent>>,
}

impl Collected {
    /// A sink that keeps everything.
    pub(crate) fn new() -> Collected {
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
    pub(crate) fn keep_control(&self) {
        let mut held = self.held();
        held.keeping = false;
        held.frames.clear();
        held.counts.clear();
    }

    /// The kept frames, and forgets them: what one step sent, not every step.
    pub(crate) fn take(&self) -> Vec<Frame> {
        std::mem::take(&mut self.held().frames)
    }

    /// How many bytes have gone out on a channel.
    pub(crate) fn count(&self, channel: u8) -> u64 {
        self.held().counts.get(&channel).copied().unwrap_or(0)
    }

    /// Forgets every count, so a case can measure one interval.
    pub(crate) fn reset_counts(&self) {
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
