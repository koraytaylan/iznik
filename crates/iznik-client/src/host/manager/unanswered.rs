//! Commands a link carried and went before answering.
//!
//! The host may have applied such a command the moment before the link went,
//! or never seen it; nothing on this side can tell which. A host that
//! remembers what it answered each client — one that advertised
//! [`Capabilities::IDENTIFY`] — settles it: this client names itself on every
//! link, and sends each such command again on the next one, under the same
//! number, to the same daemon. The host answers it from memory if it applied
//! it and applies it now if it did not, so it is applied exactly once and
//! answered like any other. A host that does not remember, or a daemon other
//! than the one the command went to, leaves the outcome unknown, and the
//! caller is told so.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::command::encode_session_command;
use iznik_protocol::identity::{ClientIdentity, CommandId, DaemonInstance};
use iznik_protocol::message::ToServer;

use crate::commands::withdraw;
use crate::host::identity::HostId;
use crate::host::manager::task::{trusted_capabilities, write};
use crate::host::manager::{ManagerEvent, Shared};
use crate::reduce::Notification;
use crate::transport::channel::RemoteChannel;

/// How far the first half of an identity is shifted to make room for the
/// second.
const HALF: u32 = u64::BITS;

/// How many identities this process has made, so two made in the same
/// nanosecond still differ.
static MADE: AtomicU64 = AtomicU64::new(0);

/// An identity nobody has picked before: two keyed hashes of the moment, this
/// process's number and how many this process has made, one for each half.
/// The keys come from the operating system's random source, so another
/// process's identities are not these.
fn fresh_identity() -> ClientIdentity {
    let moment = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let process = std::process::id();
    let half = || {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(moment);
        hasher.write_u32(process);
        hasher.write_u64(MADE.fetch_add(1, Ordering::Relaxed));
        u128::from(hasher.finish())
    };
    let high = half();
    let low = half();
    ClientIdentity(high.checked_shl(HALF).unwrap_or(0) | low)
}

/// One host task's commands whose links went before their answers.
#[derive(Debug)]
pub(super) struct Unanswered {
    /// Who this client is to the host, for as long as the task holds it:
    /// command numbers begin again with a task, so the identity does too.
    client: ClientIdentity,
    /// Whether the current link's host remembers what it answered.
    remembering: bool,
    /// The daemon the current link reached, when it said.
    instance: Option<DaemonInstance>,
    /// Commands carried on a link that went before their answers, to send
    /// again on the next link if it reaches the same daemon.
    owed: Vec<CommandId>,
}

impl Unanswered {
    /// Nothing owed, under an identity of its own.
    pub(super) fn new() -> Unanswered {
        Unanswered {
            client: fresh_identity(),
            remembering: false,
            instance: None,
            owed: Vec::new(),
        }
    }

    /// A link has been reached: names this client to a host that remembers,
    /// and sends again, under their own numbers, the commands the last link
    /// went before answering — when this link reached the daemon they went
    /// to. Those that cannot be sent again are put back and their outcome
    /// told as unknown.
    ///
    /// Gives back the commands it sent, which this link now carries.
    pub(super) async fn link_began(
        &mut self,
        host: &HostId,
        shared: &Arc<Shared>,
        channel: &mut RemoteChannel,
    ) -> Vec<CommandId> {
        let greeting = channel.greeting();
        let remembering = trusted_capabilities(greeting).contains(Capabilities::IDENTIFY)
            && greeting.instance.is_some();
        let same = remembering && greeting.instance == self.instance;
        self.instance = greeting.instance;
        self.remembering = remembering;
        let owed = std::mem::take(&mut self.owed);
        if remembering {
            let named = ToServer::Identify {
                client: self.client,
            };
            if write(channel, &named).await.is_err() {
                // A link that would not take this is one that has just died;
                // the next is the one that will carry these.
                self.owed = owed;
                return Vec::new();
            }
        }
        if !same {
            unknown(host, shared, &owed);
            return Vec::new();
        }
        let mut sent = Vec::new();
        for (at, command) in owed.iter().copied().enumerate() {
            let Some(asked) = shared
                .with(host, |view| {
                    view.awaiting(command)
                        .filter(|held| held.answered.is_none())
                        .map(|held| held.command.clone())
                })
                .flatten()
            else {
                // Answered meanwhile, or given up on and told so.
                continue;
            };
            let Ok(payload) = encode_session_command(&asked) else {
                continue;
            };
            let again = ToServer::Command {
                command_id: command,
                payload,
            };
            // Counted before the write, as every carried command is: a link
            // that fails part way through it may still have delivered it.
            sent.push(command);
            if write(channel, &again).await.is_err() {
                self.owed = owed.into_iter().skip(at.saturating_add(1)).collect();
                break;
            }
        }
        sent
    }

    /// A link has gone: what it carried and never had answered is sent again
    /// on the next link when its host remembers, and otherwise put back and
    /// told as unknown.
    pub(super) fn link_ended(
        &mut self,
        host: &HostId,
        shared: &Arc<Shared>,
        carried: &[CommandId],
    ) {
        if self.remembering {
            self.owed.extend_from_slice(carried);
        } else {
            unknown(host, shared, carried);
        }
    }
}

/// Puts back what every one of these commands showed, and says its outcome
/// is unknown.
///
/// One whose link went first may or may not have been applied — the link can
/// die between the host applying it and the answer arriving — so it is
/// neither applied nor refused nor timed out: it is unknown, and the snapshot
/// the next connection begins with is what says which.
fn unknown(host: &HostId, shared: &Arc<Shared>, carried: &[CommandId]) {
    let told: Vec<Notification> = shared
        .with(host, |view| {
            carried
                .iter()
                .copied()
                .filter(|command| withdraw(view, *command))
                .map(|command| Notification::CommandOutcomeUnknown {
                    host: host.clone(),
                    command,
                })
                .collect()
        })
        .unwrap_or_default();
    for notification in told {
        shared.publish(ManagerEvent::Notify(notification));
    }
}

/// Whether a command is still waiting to be carried: submitted, and not
/// taken back or given up on while it sat in the queue.
///
/// One that was is not sent at all. It was told to the caller as undone —
/// timed out behind a slow bootstrap, or of unknown outcome — and carrying it
/// now would apply something the caller was told did not happen.
pub(super) fn still_asked(host: &HostId, shared: &Shared, command: CommandId) -> bool {
    shared
        .with(host, |view| {
            view.awaiting(command)
                .is_some_and(|held| held.answered.is_none())
        })
        .unwrap_or(false)
}
