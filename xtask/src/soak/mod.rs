//! `xtask soak`: hours of the end-to-end stack against two fixture hosts with
//! faults, sampling memory and losing no byte.
//!
//! A system like this fails on the timescale of days and not of tests. A leak
//! of a few kilobytes per reconnection is invisible in anything that runs in a
//! minute and fatal by Thursday, so this runs the whole stack for as long as
//! it is given — one client held open against a host while another comes and
//! goes, a pane flooded, the link dropped and made good again — and watches
//! what both sides weigh while it does.
//!
//! What it fails on: a side growing past a ceiling after the warmup, never
//! being weighed at all, or last weighed long before the end; a held client
//! gone before the end, hearing nothing, hearing less than its pane was made
//! to say, or sent a screen because a reconnection could not be carried on
//! from where it was; fewer than half its rounds finishing, or more than a
//! few failing one after another; and fewer than half of them churning a
//! session. The first is the leak. The rest are all one thing — that what the
//! report says was measured really was — because a soak that measured nothing
//! is the easiest passing run there is.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iznik_harness::fixture::{Fixture, FixtureError, FixtureOptions};
use iznik_harness::process::Deadline;
use iznik_harness::staging::{STAGING_DEADLINE, stage};

pub mod commands;
pub mod judge;
pub mod report;
pub mod steps;

pub use crate::soak::commands::{COMPACT, TAIL_OUTPUT, TAIL_TROUBLE, WEIGH};
use crate::soak::commands::{cut, machine, revive, running, stop_tail, tail_command, weighed};
pub use crate::soak::judge::judged;
pub use crate::soak::report::{
    Heard, Report, Sample, grown, heard_more, minutes, poured, rendered,
};
use crate::soak::steps::{filling_step, flooding_step, opening_step, recovery_step, settled_step};

/// What this subcommand takes.
const USAGE: &str = "usage: xtask soak [--duration <minutes>] [--warmup <minutes>]";

/// How long a soak runs when nobody says: six hours, which is a working day's
/// worth of a thing that fails on the timescale of days.
pub const SOAK_DURATION: Duration = Duration::from_hours(6);

/// How much of that is not measured, because a process that has just started
/// is still growing into its work.
pub const SOAK_WARMUP: Duration = Duration::from_mins(10);

/// How much any side may grow, per hour, after the warmup.
///
/// Four mebibytes: far above what an hour of arithmetic and buffers moves on
/// a stack that is behaving, and low enough that anything growing steadily is
/// caught long before a day of it would matter.
///
/// What it is not: a floor under every leak worth finding. A soak reconnects
/// its held client a few hundred times an hour, so a leak of a kilobyte per
/// reconnection is a few hundred kilobytes an hour and passes this ceiling —
/// what catches that one is the six-hour run's series, read by a person, not
/// the ceiling. The ceiling is what fails a run without anybody looking.
pub const SOAK_GROWTH_CEILING_PER_HOUR: u64 = 4 * 1024 * 1024;

/// How often both sides are weighed, at most.
///
/// A short soak is weighed more often than this: four samples is the fewest
/// from which a growth can be read at all, so a run of a few minutes takes
/// them closer together rather than taking one at the end.
const SAMPLE_INTERVAL: Duration = Duration::from_mins(1);

/// What share of a soak's rounds have to finish for it to have been one.
///
/// Two: half of them. A round is a flood, a drop and a recovery against a
/// machine that is also building other things, and one that times out is a
/// busy machine rather than a broken stack — but a stack that has stopped
/// answering fails every round it is given, and a series taken from one is
/// exactly as flat as a series with no leak in it.
const FINISHED_SHARE: usize = 2;

/// How long before the end of a run a side's last sample may be.
///
/// Five minutes: longer than any interval a soak samples at and longer than
/// the round a sample waits behind, and short enough that a census which
/// stopped finding a side is not read as one that stopped growing.
const STALE_AFTER: Duration = Duration::from_mins(5);

/// How many rounds may fail one after another before a soak is a soak of a
/// stack that has stopped answering.
///
/// Three. A machine with other work on it drops one here and there — that is
/// what the share above is for — but a host that has gone stays gone, and
/// every round after it fails. Three in a row is a minute of nothing working.
const FAILING_STREAK: usize = 3;

/// The fewest samples a line can be fitted through, which is what growth is
/// read as.
const LEAST_SAMPLES: usize = 2;

/// How long a run has to have left after its warmup before it is held to
/// having measured anything.
///
/// The fewest samples a growth can be read from, at the interval a soak
/// samples at: below that a run cannot be asked for a rate, and above it a
/// run that reports none has a census that stopped matching rather than a
/// clock that was too short.
const MEASURABLE_SPAN: Duration = Duration::from_mins(4);

/// The fewest samples any soak takes, however short it is.
const FEWEST_SAMPLES: u32 = 4;

/// How many lines are poured through the pane before anything is measured.
///
/// A pane keeps a ring of what it has said, and a server that is filling one
/// is growing for a reason that is not a leak. Enough to fill it outright —
/// more than the four mebibytes a pane keeps — so that every sample after it
/// is of a stack at its steady state.
pub const OPENING_FLOOD_LINES: u64 = 800_000;

/// How many lines a round's flood is.
///
/// Thirty thousand of them is about two hundred kilobytes, which is most of
/// the two hundred and fifty-six kilobytes a subscription is given and less
/// than all of it. A step of the driver returns no credit, so a round that
/// poured more than its window would stall for ever against a host that was
/// doing exactly the right thing.
pub const FLOOD_LINES: u64 = 30_000;

/// How long any one thing a round waits for may take, in milliseconds.
/// Generous, because a round shares a machine with whatever else is building
/// on it, and a wait that is merely slow is not one that failed.
const PATIENCE: u64 = 120_000;

/// How long the daemon is stopped for, in seconds, and the words that say so
/// where the shell can read them.
///
/// Twenty: twice the ten seconds a client waits for a pong before it calls a
/// link gone (`ChannelOptions::default`), so that a client which had just
/// heard one still has to notice.
const DROP_SECONDS: u64 = 20;

/// How long one round of the soak's work may take: a flood, a drop, and the
/// line that has to come back after it. Generous, because a round shares a
/// machine with whatever else is building on it, and a round that is merely
/// slow is not a round that failed.
const STEP_DEADLINE: Duration = Duration::from_mins(4);

/// How many bytes three of them stand for, and how many characters carry
/// them: base 64 is four characters to three bytes, which is how a delivery's
/// length is read back out of what the held client printed.
const BASE64_BYTES: u64 = 3;

/// The other half of that ratio.
const BASE64_CHARACTERS: u64 = 4;

/// How many sides a soak weighs: the held client, the daemon whose pane it
/// holds, and the daemon where sessions are made and unmade.
pub const SIDES: usize = 3;

/// How many times the held client is asked whether it has attached yet, and
/// how long between the asking. Seven minutes in all, because reaching a host
/// is bounded by the client's own bootstrap deadline of six and one dial
/// inside that may take forty seconds on its own; a window shorter than what
/// the thing it waits for is allowed would abort a six-hour run in its first
/// minute over a slow dial.
const ATTENDING_ATTEMPTS: u32 = 210;

/// How long between those askings.
const ATTENDING_INTERVAL: Duration = Duration::from_secs(2);

/// What the held client calls the pane as it stands.
const SCREEN: &str = "screen";

/// What is typed into the middle of a marker so that what is waited for is
/// not what was typed.
///
/// A terminal echoes a line as it is typed, so a wait for a string that
/// appears in the typing is over before the shell has read it, let alone run
/// it. An empty pair of quotes is nothing to a shell and two characters to a
/// terminal: `flood""ed-7` is echoed as itself and printed as `flooded-7`.
const SPLIT: &str = "\\\"\\\"";

/// How many askings of a client's output the same answer has to come back to
/// before it has caught up, how long between them, and how many there may be
/// in all: three seconds of stillness, looked for over two minutes.
const DRAINED_FOR: u32 = 3;

/// How long between those askings.
const DRAINING_INTERVAL: Duration = Duration::from_secs(1);

/// And how many of them there may be.
const DRAINING_ATTEMPTS: u32 = 120;

/// How long one asking of whether the pane has gone quiet may take, and how
/// many askings there may be. Half a minute apiece and forty of them: twenty
/// minutes in all, which is far longer than a shell takes to pour six
/// megabytes into a ring.
const SETTLED_DEADLINE: Duration = Duration::from_secs(30);

/// How many times it is asked.
const SETTLED_ATTEMPTS: u32 = 40;

/// How many of the held client's lines are read back at a time.
///
/// A command's output is captured up to a mebibyte and the end of it is what
/// survives, so a file read in one go would be checked from its middle. A
/// line of it is about twenty bytes, so ten thousand of them is a fifth of
/// the limit even where every one of them is long.
const LINES_AT_ONCE: u64 = 10_000;

/// What a terminal puts after every line: a carriage return and a line feed.
const ENDING: u64 = 2;

/// The base the digits of a number are counted in.
const TEN: u64 = 10;

/// The most minutes either flag takes.
///
/// A year. `Duration::from_mins` is arithmetic that can overflow, and it
/// panics rather than refusing when it does, on a path whose whole job is to
/// answer a bad command line with a refusal.
const LONGEST_MINUTES: u64 = 525_600;

/// What the cut says when the lock does not name a daemon, and what it says
/// the daemon's state was while it was meant to be stopped. A stopped process
/// is `T` to the kernel, and anything else means the signal went somewhere
/// else or did not land.
const NOT_THE_DAEMON: &str = "not-the-daemon";

/// The word before that state.
const WAS: &str = "stopped:";

/// What that state has to be.
const STOPPED: &str = "stopped:T";

/// How many hosts the fixture stands up.
const HOSTS: usize = 2;

/// The host the held client watches.
const WATCHED: &str = "host0";

/// The host sessions are made and unmade on.
const CHURNED: &str = "host1";

/// What the held client is called where a container can see it.
const CLIENT_PROCESS: &str = "iznik";

/// And the daemon on a host.
pub const SERVER_PROCESS: &str = "iznik-server";

/// How much longer than the soak itself its containers may live.
///
/// Podman kills a container when its own timeout runs out, and it counts from
/// when the container started, while the soak's clock does not start until a
/// pane is open, a client is holding it and the flood that fills the ring has
/// drained. What this module allows before that: half a minute of readiness,
/// two reaches at five minutes each — a reach is a bootstrap, and a bootstrap
/// is allowed six — two steps written and run at five, seven minutes of
/// waiting for the client to attach, twenty of waiting for the flood to go
/// quiet, and a weighing apiece. What it allows after: a last round already
/// in flight at two step deadlines and a cut, a churn, and an ending whose
/// windows are half a minute each.
///
/// An hour and a half is above the sum of them for any soak worth running,
/// and a container killed while the soak believes it is running turns hours
/// of measurement into a fixture error.
const CONTAINER_MARGIN: Duration = Duration::from_mins(90);

/// How much longer than a step itself the command that runs it may take: the
/// writing of the step, the driver starting and the answer coming back.
const STEP_MARGIN: Duration = Duration::from_mins(1);

/// How long one window of the held client's output may take to read. A
/// tenth of a command's deadline, because reading ten thousand lines is
/// seconds and a six-hour run is hundreds of windows.
const WINDOW_DEADLINE: Duration = Duration::from_secs(30);

/// How long any one command in a container may take./// How long any one command in a container may take.
const COMMAND_DEADLINE: Deadline = Deadline(Duration::from_mins(5));

/// How many bytes are in a kibibyte, which is what `/proc` counts in.
const KIBIBYTE: u64 = 1024;

/// How many seconds are in an hour, for turning a growth into a rate.
const SECONDS_PER_HOUR: u64 = 3600;

/// And in a minute, for saying a duration the way a report says one.
const SECONDS_PER_MINUTE: u64 = 60;

/// Where the driver's step description is written in a container.
const STEP_INPUT: &str = "/tmp/iznik-soak-step.toml";

/// The driver, in every container.
const DRIVER: &str = "/iznik/bin/iznik-regression";

/// The command-line tool, in every container.
const TOOL: &str = "/iznik/bin/iznik";

/// Where this build's servers are, in every container.
const ARTIFACTS: &str = "/iznik/distribution";

/// Why a soak could not be run, or did not pass.
#[derive(Debug)]
pub enum SoakError {
    /// The fixture would not stand up, or a command in it failed.
    Fixture {
        /// What went wrong.
        source: FixtureError,
    },
    /// This build's artifacts could not be staged for the containers.
    Staging {
        /// What went wrong, in words.
        detail: String,
    },
    /// The command line could not be read.
    Usage {
        /// What was wrong with it.
        detail: String,
    },
    /// A side grew past the ceiling after the warmup.
    Grew {
        /// Which side.
        side: String,
        /// By how much an hour, in bytes.
        rate: u64,
    },
    /// A soak ran but proved nothing: rounds that did not finish, a held
    /// client that stopped or heard too little, a side never weighed, or a
    /// reconnection the host could not carry a client on from.
    Lost {
        /// What the driver said.
        detail: String,
    },
    /// A soak that ran and did not pass, with what it measured while it did.
    Judged {
        /// The run.
        report: Box<Report>,
        /// And what was wrong with it.
        source: Box<SoakError>,
    },
}

impl Display for SoakError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SoakError::Fixture { source } => write!(formatter, "{source}"),
            SoakError::Usage { detail } => write!(formatter, "{detail}\n{USAGE}"),
            SoakError::Grew { side, rate } => write!(
                formatter,
                "the {side} grew by {rate} bytes an hour after the warmup, and \
                 {SOAK_GROWTH_CEILING_PER_HOUR} is the most it may"
            ),
            SoakError::Staging { detail } | SoakError::Lost { detail } => {
                write!(formatter, "{detail}")
            }
            SoakError::Judged { source, .. } => write!(formatter, "{source}"),
        }
    }
}

impl core::error::Error for SoakError {}

impl From<iznik_harness::staging::StagingError> for SoakError {
    fn from(source: iznik_harness::staging::StagingError) -> SoakError {
        SoakError::Staging {
            detail: source.to_string(),
        }
    }
}

impl From<FixtureError> for SoakError {
    fn from(source: FixtureError) -> SoakError {
        SoakError::Fixture { source }
    }
}

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    let asked = match asked_for(arguments) {
        Ok(asked) => asked,
        Err(refusal) => {
            writeln!(std::io::stderr(), "{refusal}").unwrap_or_default();
            return ExitCode::from(crate::USAGE_EXIT_CODE);
        }
    };
    // The report is printed either way. Hours of samples are what tell a leak
    // from one sample that caught a flood, and a run that failed the ceiling
    // is exactly the run whose series somebody has to read.
    let (report, refusal) = match soaked(asked.0, asked.1) {
        Ok(report) => (Some(report), None),
        Err(SoakError::Judged { report, source }) => (Some(*report), Some(*source)),
        Err(refusal) => (None, Some(refusal)),
    };
    if let Some(report) = report {
        writeln!(std::io::stdout(), "{}", rendered(&report)).unwrap_or_default();
    }
    match refusal {
        None => ExitCode::SUCCESS,
        Some(refusal) => {
            writeln!(std::io::stderr(), "{refusal}").unwrap_or_default();
            ExitCode::FAILURE
        }
    }
}

/// The duration and warmup a command line asks for.
///
/// Public so that a case can ask what a command line means without running a
/// soak, which is the only other way to find out.
///
/// # Errors
///
/// [`SoakError::Usage`] for a flag this does not know or a number it cannot
/// read.
pub fn asked_for(arguments: &[OsString]) -> Result<(Duration, Duration), SoakError> {
    let (mut duration, mut warmup) = (SOAK_DURATION, SOAK_WARMUP);
    let mut rest = arguments.iter().skip(1);
    while let Some(flag) = rest.next() {
        let named = flag.to_string_lossy().into_owned();
        let minutes = rest
            .next()
            .and_then(|held| held.to_str())
            .and_then(|held| held.parse::<u64>().ok())
            .ok_or_else(|| SoakError::Usage {
                detail: format!("{named} takes a number of minutes"),
            })?;
        let minutes = minutes.min(LONGEST_MINUTES);
        match named.as_str() {
            "--duration" => duration = Duration::from_mins(minutes),
            "--warmup" => warmup = Duration::from_mins(minutes),
            _unknown => {
                return Err(SoakError::Usage {
                    detail: format!("{named} is not a flag this takes"),
                });
            }
        }
    }
    // A warmup that swallows the run leaves nothing after it, and nothing
    // after it is a growth check that says nothing and a soak that passes for
    // having measured none of itself.
    if warmup >= duration {
        return Err(SoakError::Usage {
            detail: format!(
                "a warmup of {} minutes leaves nothing of a soak of {} to measure",
                minutes(warmup),
                minutes(duration)
            ),
        });
    }
    Ok((duration, warmup))
}

/// Runs the soak and says what it found.
///
/// # Errors
///
/// [`SoakError`] for a fixture that would not stand up, a side that grew past
/// the ceiling, or a reconnection that lost a byte.
pub fn soaked(duration: Duration, warmup: Duration) -> Result<Report, SoakError> {
    let staged = stage(Deadline(STAGING_DEADLINE))?;
    // Every container is killed when podman's own timeout runs out, and the
    // default is minutes: a soak is the one thing here that outlives it, so it
    // asks for its own length and the margin the standing up, the last round
    // and the taking down at either end need.
    let mut options = FixtureOptions::new(HOSTS, staged);
    options.container_timeout = duration.saturating_add(CONTAINER_MARGIN);
    let fixture = Fixture::start(options)?;
    // Both hosts reached once before anything is measured, so that what the
    // soak watches is a stack that is up rather than one still being built.
    for host in [WATCHED, CHURNED] {
        let _reached = fixture.exec(
            "engine",
            &tool(&format!("state {host}")),
            COMMAND_DEADLINE.0,
        )?;
    }
    // A pane, and the flood that fills the ring every pane keeps. A server
    // still filling a four-mebibyte ring is growing for a reason that is not
    // a leak, so this is poured and waited out before anything is measured —
    // and before the held client attaches, because a client cannot be carried
    // along a flood six megabytes long: it would fall far enough behind for
    // the host to stop streaming to it and send the truth instead, which is
    // the one thing this soak reads as a lost byte.
    let _opened = step_run(&fixture, &opening_step(), STEP_DEADLINE)?;
    let _filled = step_run(&fixture, &filling_step(), STEP_DEADLINE)?;
    settled(&fixture)?;
    // Then the client that holds the pane for the whole soak: the one client
    // here that returns credit for every byte it takes. Nothing is measured
    // until it has said its first word, because bytes poured before it
    // attached are bytes it was never sent and could not be missing.
    let _started = fixture.exec("engine", &tail_command(), COMMAND_DEADLINE.0)?;
    attended(&fixture)?;
    let began = Instant::now();
    let mut report = Report {
        machine: machine(&fixture)?,
        duration,
        warmup,
        rounds: 0,
        floods: 0,
        cuts: 0,
        streak: 0,
        drops: 0,
        churn: 0,
        heard: Heard::default(),
        client: Vec::new(),
        server: Vec::new(),
        churned: Vec::new(),
    };
    let refused = worked(&fixture, &mut report, began);
    // Everything from here is the end of a run that has already happened, so
    // none of it may take the report with it. What went wrong is judged
    // beside what was measured, and both are printed.
    // Whatever the ending said, what it managed to count is kept: a tally
    // that stops where a stream broke is the evidence, and zeroing it would
    // print the run as one that heard nothing at all.
    let judgement =
        ending(&fixture, &mut report).and_then(|living| judged(&report, &refused, living));
    if let Err(source) = judgement {
        return Err(SoakError::Judged {
            report: Box::new(report),
            source: Box::new(source),
        });
    }
    Ok(report)
}

/// Ends the run: asks whether the held client is still there, stops it, and
/// reads what it heard.
///
/// # Errors
///
/// [`SoakError`] for a container that will not answer or a stream that says
/// the client stopped listening.
fn ending(fixture: &Fixture, report: &mut Report) -> Result<bool, SoakError> {
    // Alive at the end, and asked before it is stopped: a client that died an
    // hour in leaves a stream with no gap in it, which is what a check that
    // only looks for gaps calls whole.
    let living = alive(fixture)?;
    // Let it catch up before it is interrupted. What it is held to hearing is
    // every byte its pane was made to say, and the last round's is still
    // arriving when the clock runs out; a client stopped mid-delivery drops
    // what was queued and the run reports a loss of its own making.
    drained(fixture)?;
    let _stopped = fixture.exec("engine", &stop_tail(), COMMAND_DEADLINE.0)?;
    let (heard, broken) = heard_by(fixture)?;
    report.heard = heard;
    match broken {
        None => Ok(living),
        Some(detail) => Err(SoakError::Lost { detail }),
    }
}

/// Waits for the held client to have attached, or says it never did.
///
/// What it writes first is the pane as it stands, which is what a host sends
/// a client that has just attached; until that line is there, the client is
/// still reaching the host. That it is that line and not some other is what
/// the count of screens later rests on: one is the attachment, and a run that
/// never saw it would read one real loss as a healthy run.
///
/// # Errors
///
/// [`SoakError::Lost`] when it never attaches, with whatever it said about
/// why, and [`SoakError::Fixture`] when the container will not say.
fn attended(fixture: &Fixture) -> Result<(), SoakError> {
    for _attempt in 0..ATTENDING_ATTEMPTS {
        let said = fixture.exec(
            "engine",
            &format!("head -n 1 {TAIL_OUTPUT} 2>/dev/null"),
            COMMAND_DEADLINE.0,
        )?;
        let first = String::from_utf8_lossy(&said.stdout).trim().to_owned();
        if first.starts_with(SCREEN) {
            return Ok(());
        }
        if !first.is_empty() {
            return Err(SoakError::Lost {
                detail: format!(
                    "the held client's first word was not the pane as it stands: {first}"
                ),
            });
        }
        std::thread::sleep(ATTENDING_INTERVAL);
    }
    let trouble = fixture.exec("engine", &format!("cat {TAIL_TROUBLE}"), COMMAND_DEADLINE.0)?;
    Err(SoakError::Lost {
        detail: format!(
            "the held client never attached to the pane; it said: {}",
            String::from_utf8_lossy(&trouble.stdout).trim()
        ),
    })
}

/// Waits for the flood poured before the clock starts to be over.
///
/// Asked by typing a line and waiting for the shell to print it: a shell runs
/// what it is given in the order it is given, so the line comes back only
/// once the flood before it has been produced. Each asking is a client of its
/// own, because a client that asked while the flood was still pouring has its
/// window filled by it and cannot be carried on — so a failed asking is not a
/// failure, it is the answer "not yet", and the next one starts fresh.
///
/// # Errors
///
/// [`SoakError::Lost`] when the pane never goes quiet, with what the last
/// asking said.
fn settled(fixture: &Fixture) -> Result<(), SoakError> {
    let mut refused = String::new();
    for _attempt in 0..SETTLED_ATTEMPTS {
        match step_run(fixture, &settled_step(), SETTLED_DEADLINE) {
            Ok(_said) => return Ok(()),
            Err(refusal) => refused = refusal.to_string(),
        }
    }
    Err(SoakError::Lost {
        detail: format!("the pane never went quiet; the last asking said: {refused}"),
    })
}

/// Waits for the held client to have caught up.
///
/// Its output stops growing when it has taken everything the pane has said.
/// Not an error when it does not: a client that is still hearing at the end
/// of its patience is judged on what it heard by then, and the judgement is
/// the place that says whether that was enough.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say how much it has
/// written.
fn drained(fixture: &Fixture) -> Result<(), SoakError> {
    let (mut said, mut still) = (0_u64, 0_u32);
    for _attempt in 0..DRAINING_ATTEMPTS {
        let now = lines(fixture)?;
        still = if now == said {
            still.saturating_add(1)
        } else {
            0
        };
        said = now;
        if still >= DRAINED_FOR {
            return Ok(());
        }
        std::thread::sleep(DRAINING_INTERVAL);
    }
    Ok(())
}

/// How many lines the held client has written.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say.
fn lines(fixture: &Fixture) -> Result<u64, SoakError> {
    let said = fixture.exec(
        "engine",
        &format!("wc -l < {TAIL_OUTPUT}"),
        COMMAND_DEADLINE.0,
    )?;
    Ok(String::from_utf8_lossy(&said.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0))
}

/// Whether the held client is still running.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say.
fn alive(fixture: &Fixture) -> Result<bool, SoakError> {
    let said = fixture.exec("engine", &running(CLIENT_PROCESS), COMMAND_DEADLINE.0)?;
    // Read as a number and not as anything that is not a zero: a census that
    // answered with a warning, with nothing, or with half a number is a
    // census that did not find the client, and calling that alive is the
    // reading that passes a run whose client died.
    let found = String::from_utf8_lossy(&said.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    Ok(found > 0)
}

/// What the held client heard, read back a window of lines at a time.
///
/// A command's output is captured up to a limit and the end of it is what
/// survives, so a file bigger than that read in one go would be checked from
/// its middle and its beginning called whole. The windows are small enough
/// that none of them reaches the limit, and they are read in order, so the
/// reckoning carries from one to the next.
///
/// Answers with what was counted and, where the stream broke, the words for
/// it — never one without the other.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say.
fn heard_by(fixture: &Fixture) -> Result<(Heard, Option<String>), SoakError> {
    let counted = lines(fixture)?;
    let mut heard = Heard::default();
    let mut from = 1;
    while from <= counted {
        let to = from.saturating_add(LINES_AT_ONCE).saturating_sub(1);
        let window = fixture.exec(
            "engine",
            &format!("sed -n '{from},{to}p;{to}q' {TAIL_OUTPUT}"),
            WINDOW_DEADLINE,
        )?;
        // A window that says the stream broke ends the reading and not the
        // report: what was counted up to there is what the client heard, and
        // it is the number a person needs beside the words.
        if let Err(detail) = heard_more(&mut heard, &String::from_utf8_lossy(&window.stdout)) {
            return Ok((heard, Some(detail)));
        }
        from = to.saturating_add(1);
    }
    Ok((heard, None))
}

/// Runs rounds back to back until the clock says stop, weighing every side on
/// a schedule of its own.
///
/// The work has no schedule and the sampling does: a leak is found by working
/// and not by waiting, and a series is only a series if its points are evenly
/// spaced. Answers with what the last round to fail said, for the judgement
/// made once at the end about how many of them did.
fn worked(fixture: &Fixture, report: &mut Report, began: Instant) -> String {
    let interval = interval_for(report.duration);
    let (mut refused, mut weigh_at, mut failing) = (String::new(), Duration::ZERO, 0_usize);
    while began.elapsed() < report.duration {
        // One round: a flood poured through the pane and waited for, the
        // daemon stopped underneath it for longer than any deadline either
        // client keeps, started again, and a host that has to answer
        // afterwards.
        report.rounds = report.rounds.saturating_add(1);
        match round(fixture, report) {
            Ok(()) => {
                report.drops = report.drops.saturating_add(1);
                failing = 0;
            }
            Err(refusal) => {
                refused = refusal.to_string();
                failing = failing.saturating_add(1);
                report.streak = report.streak.max(failing);
                writeln!(
                    std::io::stdout(),
                    "round {} did not finish: {refused}",
                    report.rounds
                )
                .unwrap_or_default();
            }
        }
        match fixture.exec(
            "engine",
            &tool(&format!("benchmark {CHURNED}")),
            COMMAND_DEADLINE.0,
        ) {
            Ok(_churned) => report.churn = report.churn.saturating_add(1),
            Err(refusal) => {
                writeln!(std::io::stdout(), "the churn did not run: {refusal}").unwrap_or_default();
            }
        }
        let at = began.elapsed();
        if at < weigh_at {
            continue;
        }
        weigh_at = at.saturating_add(interval);
        weigh(fixture, report, at);
    }
    refused
}

/// One round of the soak's work.
///
/// # Errors
///
/// [`SoakError::Lost`] when a step refuses, and [`SoakError::Fixture`] when
/// the container will not run the cut.
fn round(fixture: &Fixture, report: &mut Report) -> Result<(), SoakError> {
    let _flooded = step_run(fixture, &flooding_step(report.rounds), STEP_DEADLINE)?;
    // Counted here rather than at the end of the round: the flood happened,
    // whatever becomes of the rest of it, and what the held client is held to
    // hearing is what its pane was really made to say.
    report.floods = report.floods.saturating_add(1);
    let cut = fixture.exec(WATCHED, &cut(), COMMAND_DEADLINE.0);
    let said = match cut {
        Ok(said) => String::from_utf8_lossy(&said.stdout).into_owned(),
        Err(source) => {
            // The cut stops the daemon and starts it again in one command, so
            // a soak that dies between the two leaves nothing frozen — but a
            // command that failed may have got as far as the stopping.
            let _revived = fixture.exec(WATCHED, &revive(), COMMAND_DEADLINE.0);
            return Err(SoakError::Fixture { source });
        }
    };
    if !said.contains(STOPPED) {
        let _revived = fixture.exec(WATCHED, &revive(), COMMAND_DEADLINE.0);
        return Err(SoakError::Lost {
            detail: format!("the cut did not stop the daemon; it said: {}", said.trim()),
        });
    }
    report.cuts = report.cuts.saturating_add(1);
    let _recovered = step_run(fixture, &recovery_step(report.rounds), STEP_DEADLINE)?;
    Ok(())
}

/// Weighs every side once, saying what each of them came to.
///
/// A weighing that fails or comes back as nothing is said and not kept: a
/// resident size of zero is a process that was not found, and a series with a
/// zero in it would read as a recovery from a leak. Whether a side was ever
/// weighed at all is the judgement, made once at the end — losing a whole
/// soak to one loaded machine's failed weighing would be worse than the
/// sample.
fn weigh(fixture: &Fixture, report: &mut Report, at: Duration) {
    let weighing = [
        (&mut report.client, "engine", CLIENT_PROCESS),
        (&mut report.server, WATCHED, SERVER_PROCESS),
        (&mut report.churned, CHURNED, SERVER_PROCESS),
    ];
    for (series, container, process) in weighing {
        let said = match weighed(fixture, container, process, at) {
            Ok(sample) if sample.bytes > 0 => {
                series.push(sample);
                format!("{} bytes", sample.bytes)
            }
            Ok(_nothing) => "nothing, so it was not found".to_owned(),
            Err(refusal) => format!("nothing: {refusal}"),
        };
        writeln!(
            std::io::stdout(),
            "sample at {}s in {container}: {said} ({process})",
            at.as_secs()
        )
        .unwrap_or_default();
    }
}

/// A command of the tool, with this build's servers where it can find them.
fn tool(rest: &str) -> String {
    format!("IZNIK_ARTIFACTS_DIRECTORY={ARTIFACTS} {TOOL} {rest}")
}

/// How often a soak of this length weighs both sides.
///
/// Never longer than a minute, and never so long that a run takes fewer than
/// [`FEWEST_SAMPLES`] of them: a growth read from one sample is not a growth.
fn interval_for(duration: Duration) -> Duration {
    SAMPLE_INTERVAL.min(
        duration
            .checked_div(FEWEST_SAMPLES)
            .unwrap_or(SAMPLE_INTERVAL),
    )
}

/// Runs one step of the driver in the engine and says what it answered.
///
/// The command is given the step's own deadline and a little besides, so that
/// a step which is allowed twenty minutes is not cut off at five by the
/// command that runs it.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the step cannot be written or run, and
/// [`SoakError::Lost`] when the driver refuses it.
fn step_run(fixture: &Fixture, step: &str, deadline: Duration) -> Result<String, SoakError> {
    let write = format!("cat > {STEP_INPUT} <<'SOAK'\n{step}\nSOAK\n");
    let _written = fixture.exec("engine", &write, COMMAND_DEADLINE.0)?;
    // Under the step's own deadline and not one command's: the driver is
    // given a step that may take twenty minutes, and an exec cut off at five
    // would report a hang where there was a flood.
    let done = fixture.exec(
        "engine",
        &format!("IZNIK_ARTIFACTS_DIRECTORY={ARTIFACTS} {DRIVER} step < {STEP_INPUT}"),
        deadline.saturating_add(STEP_MARGIN),
    )?;
    let said = String::from_utf8_lossy(&done.stdout).into_owned();
    if said.contains("\"exit\":0") {
        return Ok(said);
    }
    Err(SoakError::Lost { detail: said })
}
