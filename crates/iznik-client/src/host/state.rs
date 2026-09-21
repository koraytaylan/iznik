//! A host's life, as a table.
//!
//! Probing, bootstrapping, connected, reconnecting behind a growing wait,
//! failed with a time to try again — all of it decided here, where it is a
//! pure function of a state, an event and a moment, and none of it discovered
//! in the manager under a link that has just died. What happens when a laptop
//! closes on four hosts at once is a property of this file.
//!
//! The wait grows and it is jittered, and the jitter is seeded rather than
//! random, because a state machine that reads a clock or a random device is
//! one no table can be written for. The manager gives each host a seed of its
//! own; that, and not chance, is what keeps four hosts that failed together
//! from coming back together.

use core::fmt::{self, Display, Formatter};
use core::time::Duration;
use std::time::Instant;

use iznik_protocol::capabilities::Capabilities;

use crate::bootstrap::launch::Stage;
use crate::bootstrap::probe::InstalledServer;
use crate::host::identity::HostId;

/// How long a host waits before its first retry.
pub const BACKOFF_INITIAL: Duration = Duration::from_secs(1);

/// The longest it ever waits.
pub const BACKOFF_MAXIMUM: Duration = Duration::from_mins(1);

/// The seed a policy carries when nothing gives it one.
pub const BACKOFF_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// The odd constant `SplitMix64` walks its state by.
const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// Its first mixing multiplier.
const FIRST_MIX: u64 = 0xbf58_476d_1ce4_e5b9;

/// Its second.
const SECOND_MIX: u64 = 0x94d0_49bb_1331_11eb;

/// Its first shift.
const FIRST_SHIFT: u32 = 30;

/// Its second.
const SECOND_SHIFT: u32 = 27;

/// Its last.
const THIRD_SHIFT: u32 = 31;

/// The number the wait is doubled by, and the number it is halved by.
const DOUBLING: u32 = 2;

/// The attempt a first failure is.
const FIRST_ATTEMPT: u32 = 1;

/// How long to wait, and how far apart to keep hosts that failed together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// The wait after the first failure, before jitter.
    pub initial: Duration,
    /// The longest wait, which no jittered delay ever exceeds.
    pub maximum: Duration,
    /// What the jitter is drawn from.
    ///
    /// Part of the policy rather than of the machine, so that a host's whole
    /// behaviour is a function of what it was given and a test needs no clock
    /// and no random device. The manager derives one per host from the alias.
    pub seed: u64,
}

impl Default for BackoffPolicy {
    fn default() -> BackoffPolicy {
        BackoffPolicy {
            initial: BACKOFF_INITIAL,
            maximum: BACKOFF_MAXIMUM,
            seed: BACKOFF_SEED,
        }
    }
}

/// Why replacing the server on a host is on offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeReason {
    /// The host runs a different version of the server than this build
    /// carries.
    Version,
    /// The host runs the same version but its server is missing capabilities
    /// this build knows, so features gated on them are unavailable until it is
    /// replaced.
    Capabilities,
}

/// A server this build could put on a host, offered rather than installed.
///
/// The daemon *is* the sessions, so replacing it ends every one of them; that
/// is why this is offered and never done quietly, whichever reason put it here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeOffer {
    /// What the host is running.
    pub installed: InstalledServer,
    /// What this build would put there.
    pub bundled: InstalledServer,
    /// Why it is on offer.
    pub reason: UpgradeReason,
}

impl UpgradeOffer {
    /// One sentence saying what is on offer and why, naming the host it is
    /// about.
    #[must_use]
    pub fn summary(&self, host: &HostId) -> String {
        match self.reason {
            UpgradeReason::Version => format!(
                "upgrade {host} from iznik {} to iznik {}",
                self.installed.crate_version, self.bundled.crate_version
            ),
            UpgradeReason::Capabilities => format!(
                "upgrade {host}: its iznik {} is missing features this build has",
                self.installed.crate_version
            ),
        }
    }
}

/// Where a host is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostState {
    /// Nothing is being done about it.
    Disconnected,
    /// It is being asked what it is.
    Probing,
    /// Something is being put on it.
    Bootstrapping {
        /// Which part of the bootstrap is running.
        stage: Stage,
    },
    /// Its server is being started and greeted.
    Connecting,
    /// Its server is being replaced with this build's.
    ///
    /// Distinct from [`HostState::Connecting`] because a person asked for it
    /// and it ends the sessions the host holds: the window says which of the
    /// two it is waiting through.
    Upgrading,
    /// It is connected.
    Connected {
        /// What its server says it is.
        server_version: String,
        /// What its server advertised it can decode, so a surface offers only
        /// what this server will answer.
        capabilities: Capabilities,
        /// A newer one, when this build carries one.
        upgrade: Option<UpgradeOffer>,
    },
    /// Its link went, and it will be tried again.
    Reconnecting {
        /// How many times it has failed since it was last connected.
        attempt: u32,
        /// What went wrong the last time, when it was not simply a link that
        /// went quiet — so that a host failing for good says why rather than
        /// counting for ever.
        trouble: Option<String>,
        /// When it will be tried again.
        retry_at: Instant,
    },
    /// It could not be reached, and it will be tried again.
    Failed {
        /// What went wrong, in the words whatever failed used.
        error: String,
        /// When it will be tried again.
        retry_at: Instant,
    },
}

impl HostState {
    /// Whether this state is a connection whose server can reorder sessions.
    ///
    /// False for every state but [`HostState::Connected`], and false for a
    /// connection whose server did not advertise [`Capabilities::REORDER_SESSIONS`]
    /// — a build that predates the command and would refuse its frame.
    #[must_use]
    pub fn reorders_sessions(&self) -> bool {
        match self {
            HostState::Connected { capabilities, .. } => {
                capabilities.bits() & Capabilities::REORDER_SESSIONS.bits() != 0
            }
            _otherwise => false,
        }
    }

    /// What this connection's server is missing of the capabilities that gate
    /// a person's features, when it is connected.
    ///
    /// Empty for a server of this build, for one newer, for one missing only
    /// compression or resume, and for every state but [`HostState::Connected`]
    /// — nothing has said what a host that is not connected can do.
    #[must_use]
    pub fn missing_capabilities(&self) -> Capabilities {
        match self {
            HostState::Connected { capabilities, .. } => capabilities.missing_features(),
            _otherwise => Capabilities::from_bits(0),
        }
    }
}

impl Display for HostState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            HostState::Disconnected => formatter.write_str("disconnected"),
            HostState::Probing => formatter.write_str("probing"),
            HostState::Bootstrapping { stage } => write!(formatter, "bootstrapping, {stage}"),
            HostState::Connecting => formatter.write_str("connecting"),
            HostState::Upgrading => formatter.write_str("upgrading its server"),
            HostState::Connected { server_version, .. } => {
                write!(formatter, "connected to {server_version}")
            }
            HostState::Reconnecting {
                attempt,
                trouble: None,
                ..
            } => write!(formatter, "reconnecting, attempt {attempt}"),
            HostState::Reconnecting {
                attempt,
                trouble: Some(said),
                ..
            } => write!(formatter, "reconnecting, attempt {attempt}: {said}"),
            HostState::Failed { error, .. } => write!(formatter, "failed: {error}"),
        }
    }
}

/// Something that happened to a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// Somebody asked for it — added it, or asked for it to be tried now.
    Added,
    /// Its bootstrap reached a stage.
    Reached {
        /// The stage.
        stage: Stage,
    },
    /// It is connected, and this is what answered.
    Connected {
        /// What its server says it is.
        server_version: String,
        /// What its server advertised it can decode.
        capabilities: Capabilities,
        /// A newer one, when this build carries one.
        upgrade: Option<UpgradeOffer>,
    },
    /// Its bootstrap did not finish.
    Failed {
        /// What went wrong.
        error: String,
    },
    /// The link to a connected host stopped answering.
    LinkDead {
        /// What the channel said.
        detail: String,
    },
    /// The moment a retry was scheduled for has come.
    RetryDue,
    /// Somebody asked for its server to be replaced with this build's.
    ///
    /// Deliberately not a removal and an addition: the daemon is being
    /// replaced, not the host, so the model, the selection and every held
    /// order stay exactly where they are and the reconnect that follows
    /// resumes from the byte each pane holds.
    UpgradeAsked,
    /// Somebody asked for it to go.
    Removed,
}

/// Something the manager must do about what happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Begin the bootstrap: probe, install if the host needs it, launch, hand
    /// shake.
    Bootstrap,
    /// End a bootstrap that is still running.
    StopBootstrap,
    /// Close the channel.
    CloseChannel,
    /// Resume every subscription at the byte the model holds, which is what
    /// carries a pane's stream across a drop.
    Resume,
    /// Send [`HostEvent::RetryDue`] when this moment comes.
    RetryAt(Instant),
    /// Forget everything about the host.
    Forget,
}

/// One host's state, and the rule that moves it.
#[derive(Clone, Debug)]
pub struct HostStateMachine {
    /// Where it is.
    state: HostState,
    /// How long it waits, and what its jitter is drawn from.
    policy: BackoffPolicy,
    /// How many times it has failed since it was last connected.
    failures: u32,
    /// Whether it has been connected since it was last asked for.
    ///
    /// What tells a host coming back from one that never arrived: the first
    /// keeps counting its attempts in [`HostState::Reconnecting`], the second
    /// waits in [`HostState::Failed`]. Without it every attempt after the
    /// first would land in `Failed`, and `Reconnecting` could only ever say
    /// "attempt 1".
    reconnecting: bool,
    /// The jitter's own state, walked once per delay.
    drawn: u64,
}

impl HostStateMachine {
    /// A host nothing is being done about.
    #[must_use]
    pub fn new(backoff: BackoffPolicy) -> HostStateMachine {
        HostStateMachine {
            state: HostState::Disconnected,
            drawn: backoff.seed,
            policy: backoff,
            failures: 0,
            reconnecting: false,
        }
    }

    /// Where it is.
    #[must_use]
    pub fn state(&self) -> &HostState {
        &self.state
    }

    /// How long it waits after this many failures, jittered.
    ///
    /// The wait doubles from [`BackoffPolicy::initial`] and stops at
    /// `maximum`; the jitter is drawn from the half of it below that, so a
    /// delay is always between half the wait and the whole of it, and never
    /// past the maximum.
    fn delay(&mut self) -> Duration {
        let steps = self.failures.saturating_sub(FIRST_ATTEMPT);
        let doubled = self
            .policy
            .initial
            .saturating_mul(DOUBLING.saturating_pow(steps.min(u32::BITS)));
        let base = doubled.min(self.policy.maximum);
        let whole = u64::try_from(base.as_nanos()).unwrap_or(u64::MAX);
        let half = whole.checked_div(u64::from(DOUBLING)).unwrap_or(whole);
        let spread = half
            .checked_add(1)
            .map_or(0, |band| self.draw().checked_rem(band).unwrap_or(0));
        Duration::from_nanos(half.saturating_add(spread))
    }

    /// The next number from the seeded source, by `SplitMix64`.
    fn draw(&mut self) -> u64 {
        self.drawn = self.drawn.wrapping_add(GOLDEN);
        let once = (self.drawn ^ (self.drawn >> FIRST_SHIFT)).wrapping_mul(FIRST_MIX);
        let again = (once ^ (once >> SECOND_SHIFT)).wrapping_mul(SECOND_MIX);
        again ^ (again >> THIRD_SHIFT)
    }

    /// The moment to try again after one more failure, and the action that
    /// schedules it.
    fn schedule(&mut self, now: Instant) -> Instant {
        self.failures = self.failures.saturating_add(1);
        let waiting = self.delay();
        now.checked_add(waiting).unwrap_or(now)
    }

    /// What a bootstrap's stage means for where the host is.
    fn reached(stage: Stage) -> HostState {
        match stage {
            Stage::Probe => HostState::Probing,
            Stage::Upload => HostState::Bootstrapping { stage },
            // Starting the server and greeting it are one thing from outside:
            // the host is being connected to.
            Stage::Launch | Stage::Handshake => HostState::Connecting,
        }
    }

    /// The teardown a state needs before the host is forgotten.
    fn teardown(state: &HostState) -> Vec<Action> {
        match state {
            HostState::Probing
            | HostState::Bootstrapping { .. }
            | HostState::Connecting
            | HostState::Upgrading => {
                vec![Action::StopBootstrap, Action::Forget]
            }
            HostState::Connected { .. } => vec![Action::CloseChannel, Action::Forget],
            HostState::Disconnected | HostState::Reconnecting { .. } | HostState::Failed { .. } => {
                vec![Action::Forget]
            }
        }
    }

    /// Tears down whatever the host was holding and forgets everything about
    /// it, so that adding it again is adding it for the first time.
    ///
    /// The count and the reason go with it: a host somebody removed and added
    /// again is not one that has been failing, and a backoff carried over
    /// would have it wait minutes for a first attempt.
    fn forget(&mut self) -> Vec<Action> {
        let torn = HostStateMachine::teardown(&self.state);
        self.state = HostState::Disconnected;
        self.reconnecting = false;
        self.failures = 0;
        torn
    }

    /// Begins a bootstrap, whatever the host was doing before.
    fn begin(&mut self) -> Vec<Action> {
        self.state = HostState::Probing;
        vec![Action::Bootstrap]
    }

    /// Puts the host into a wait with a time to try again.
    ///
    /// Which wait it is depends on whether the host was ever reached: one that
    /// was is reconnecting, and its attempts are counted; one that never was
    /// has failed, and what matters about it is why.
    fn hold(&mut self, error: String, now: Instant) -> Vec<Action> {
        let retry_at = self.schedule(now);
        self.state = if self.reconnecting {
            HostState::Reconnecting {
                attempt: self.failures,
                trouble: Some(error),
                retry_at,
            }
        } else {
            HostState::Failed { error, retry_at }
        };
        vec![Action::RetryAt(retry_at)]
    }

    /// A host nothing is being done about.
    fn idle(&mut self, event: &HostEvent) -> Vec<Action> {
        match event {
            HostEvent::Added => self.begin(),
            HostEvent::Removed => vec![Action::Forget],
            _otherwise => Vec::new(),
        }
    }

    /// A host being probed, bootstrapped or connected to.
    fn starting(&mut self, event: HostEvent, now: Instant) -> Vec<Action> {
        match event {
            HostEvent::Reached { stage } => {
                self.state = HostStateMachine::reached(stage);
                Vec::new()
            }
            HostEvent::Connected {
                server_version,
                capabilities,
                upgrade,
            } => {
                self.failures = 0;
                self.reconnecting = false;
                self.state = HostState::Connected {
                    server_version,
                    capabilities,
                    upgrade,
                };
                // Every subscription the model holds is resumed at the byte it
                // holds, which is what carries a pane across a drop.
                vec![Action::Resume]
            }
            HostEvent::Failed { error } | HostEvent::LinkDead { detail: error } => {
                self.hold(error, now)
            }
            HostEvent::Removed => self.forget(),
            // It is already being tried, and a replacement asked for now is
            // already being honoured by whatever is running: asking again
            // changes nothing, and a retry that fires late must not start a
            // second bootstrap.
            HostEvent::Added | HostEvent::RetryDue | HostEvent::UpgradeAsked => Vec::new(),
        }
    }

    /// A connected host.
    fn connected(&mut self, event: HostEvent, now: Instant) -> Vec<Action> {
        match event {
            HostEvent::UpgradeAsked => {
                // The task that heard the ask closes this channel and runs the
                // replacement itself, so there is no action to take here. The
                // host is not forgotten: the model, the selection and every
                // queued order stay put, so what a still-drawing window asks
                // for meanwhile is carried once the new link is up.
                self.state = HostState::Upgrading;
                Vec::new()
            }
            HostEvent::LinkDead { .. } => {
                self.reconnecting = true;
                let retry_at = self.schedule(now);
                self.state = HostState::Reconnecting {
                    attempt: self.failures,
                    trouble: None,
                    retry_at,
                };
                vec![Action::CloseChannel, Action::RetryAt(retry_at)]
            }
            HostEvent::Failed { error } => {
                // It was connected, whatever went wrong: it is coming back,
                // not arriving for the first time.
                self.reconnecting = true;
                let mut held = vec![Action::CloseChannel];
                held.extend(self.hold(error, now));
                held
            }
            HostEvent::Connected {
                server_version,
                capabilities,
                upgrade,
            } => {
                self.state = HostState::Connected {
                    server_version,
                    capabilities,
                    upgrade,
                };
                Vec::new()
            }
            HostEvent::Removed => self.forget(),
            HostEvent::Added | HostEvent::Reached { .. } | HostEvent::RetryDue => Vec::new(),
        }
    }

    /// A host waiting to be tried again.
    fn waiting(&mut self, event: &HostEvent) -> Vec<Action> {
        match event {
            // A person asking for it now does not wait out the backoff.
            HostEvent::RetryDue | HostEvent::Added => self.begin(),
            // Replacing the server of a host that is not connected reaches it
            // the same way an upgrade does: the bootstrap runs and whatever is
            // there is replaced. There is no live daemon to stop.
            HostEvent::UpgradeAsked => {
                self.state = HostState::Upgrading;
                Vec::new()
            }
            HostEvent::Removed => self.forget(),
            _otherwise => Vec::new(),
        }
    }

    /// Moves the host, and says what must be done about it.
    ///
    /// Everything time-dependent is `now`: nothing here reads a clock, which
    /// is what lets the whole of it be a table.
    pub fn on(&mut self, event: HostEvent, now: Instant) -> Vec<Action> {
        match self.state {
            HostState::Disconnected => self.idle(&event),
            HostState::Probing
            | HostState::Bootstrapping { .. }
            | HostState::Connecting
            | HostState::Upgrading => self.starting(event, now),
            HostState::Connected { .. } => self.connected(event, now),
            HostState::Reconnecting { .. } | HostState::Failed { .. } => self.waiting(&event),
        }
    }
}
