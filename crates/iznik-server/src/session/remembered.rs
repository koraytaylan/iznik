//! What the host answered each client's recent commands, kept across
//! connections so a command re-sent after a dropped link is answered rather
//! than applied a second time.
//!
//! A client numbers its commands and names itself with an `Identify`. A link
//! can die between the host applying a command and its answer arriving, and
//! the client cannot tell that from a command that never arrived. So it sends
//! the command again on its next connection, under the same number, and the
//! host — finding the number here for that client — sends back the answer it
//! gave the first time. Nothing is applied twice.
//!
//! The memory is bounded three ways: so many clients, so many commands each,
//! and an age past which an answer is forgotten. A client re-sends what it is
//! still waiting on as soon as it reconnects, and gives a command up after a
//! few seconds unanswered, so what is forgotten is what nobody will ask for.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use iznik_protocol::identity::{ClientIdentity, CommandId};

/// How many clients' answers are kept. A client is one application's hold on
/// one host; a few dozen is more than one person keeps connected at once, and
/// the one forgotten first is the one that has been quiet longest.
pub const REMEMBERED_CLIENTS: usize = 64;

/// How many answers are kept for one client: its most recent commands. A
/// client waits on a handful at a time, and a person does not type sixty-four
/// commands in the seconds a reconnect takes.
pub const REMEMBERED_COMMANDS: usize = 64;

/// How long an answer is kept. A client gives a command up well before this
/// and never re-sends one it gave up on; this is what keeps a client that
/// never comes back from holding memory for ever.
pub const REMEMBERED_FOR: Duration = Duration::from_mins(10);

/// One answer given.
#[derive(Debug)]
struct Answered {
    /// The client's number for the command.
    command: CommandId,
    /// The encoded outcome, exactly as it was sent.
    outcome: Vec<u8>,
    /// When it was given.
    at: Instant,
}

/// The answers the host gave each client's recent commands.
#[derive(Debug, Default)]
pub struct RememberedCommands {
    /// Each client's answers, oldest first.
    clients: BTreeMap<ClientIdentity, VecDeque<Answered>>,
}

impl RememberedCommands {
    /// The answer given to `command` from `client`, if it is still held.
    pub fn recall(
        &mut self,
        client: ClientIdentity,
        command: CommandId,
        now: Instant,
    ) -> Option<Vec<u8>> {
        self.forget_before(now);
        self.clients
            .get(&client)?
            .iter()
            .find(|answered| answered.command == command)
            .map(|answered| answered.outcome.clone())
    }

    /// Keeps the answer given to `command` from `client`, forgetting that
    /// client's oldest when it holds too many and the quietest client when
    /// there are too many.
    pub fn remember(
        &mut self,
        client: ClientIdentity,
        command: CommandId,
        outcome: Vec<u8>,
        now: Instant,
    ) {
        self.forget_before(now);
        let answers = self.clients.entry(client).or_default();
        answers.push_back(Answered {
            command,
            outcome,
            at: now,
        });
        while answers.len() > REMEMBERED_COMMANDS {
            let _oldest = answers.pop_front();
        }
        while self.clients.len() > REMEMBERED_CLIENTS {
            let quietest = self
                .clients
                .iter()
                .min_by_key(|(_held, kept)| kept.back().map(|answered| answered.at))
                .map(|(held, _kept)| *held);
            let Some(quietest) = quietest else {
                break;
            };
            let _forgotten = self.clients.remove(&quietest);
        }
    }

    /// Forgets every answer older than [`REMEMBERED_FOR`], and every client
    /// left with none.
    fn forget_before(&mut self, now: Instant) {
        let Some(cutoff) = now.checked_sub(REMEMBERED_FOR) else {
            return;
        };
        for answers in self.clients.values_mut() {
            while answers.front().is_some_and(|answered| answered.at < cutoff) {
                let _aged = answers.pop_front();
            }
        }
        self.clients.retain(|_client, answers| !answers.is_empty());
    }
}
