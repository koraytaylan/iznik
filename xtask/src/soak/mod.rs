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
//! What it fails on: a side growing past a ceiling after the warmup or never
//! being weighed at all; a held client gone before the end, hearing less than
//! its pane was made to say, or sent a screen because a reconnection could not
//! be carried on from where it was; and fewer than half its rounds finishing.
//! The first is the leak. The rest are all one thing — that what the report
//! says was measured really was — because a soak that measured nothing is the
//! easiest passing run there is.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iznik_harness::fixture::{Fixture, FixtureError, FixtureOptions};
use iznik_harness::process::Deadline;
use iznik_harness::staging::{STAGING_DEADLINE, stage};

pub mod report;
pub mod steps;

pub use crate::soak::report::{
    Heard, Report, Sample, grown, heard_more, minutes, poured, rendered,
};
use crate::soak::steps::{flooding_step, opening_step, recovery_step, settling_step};

/// What this subcommand takes.
const USAGE: &str = "usage: xtask soak [--duration <minutes>] [--warmup <minutes>]";

/// How long a soak runs when nobody says: six hours, which is a working day's
/// worth of a thing that fails on the timescale of days.
pub const SOAK_DURATION: Duration = Duration::from_hours(6);

/// How much of that is not measured, because a process that has just started
/// is still growing into its work.
pub const SOAK_WARMUP: Duration = Duration::from_mins(10);

/// How much either side may grow, per hour, after the warmup.
///
/// Four mebibytes: far above what an hour of arithmetic and buffers moves, and
/// far below what a kilobyte a reconnection would reach in a day.
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

/// Two, for the halves a measured series is cut into and the middle of each.
const HALVES: usize = 2;

/// The fewest samples any soak takes, however short it is.
const FEWEST_SAMPLES: u32 = 4;

/// How many lines are poured through the pane before anything is measured.
///
/// A pane keeps a ring of what it has said, and a server that is filling one
/// is growing for a reason that is not a leak. Enough to fill it outright —
/// more than the four mebibytes a pane keeps — so that every sample after it
/// is of a stack at its steady state.
const OPENING_FLOOD_LINES: u64 = 800_000;

/// How many lines a round's flood is.
///
/// Thirty thousand of them is about two hundred kilobytes, which is most of
/// the two hundred and fifty-six kilobytes a subscription is given and less
/// than all of it. A step of the driver returns no credit, so a round that
/// poured more than its window would stall for ever against a host that was
/// doing exactly the right thing.
const FLOOD_LINES: u64 = 30_000;

/// How long any one thing a round waits for may take, in milliseconds.
/// Generous, because a round shares a machine with whatever else is building
/// on it, and a wait that is merely slow is not one that failed.
const PATIENCE: u64 = 120_000;

/// And how long the flood poured before the clock starts may take. Longer,
/// because it is twenty-six times the size of a round's.
const SETTLING_PATIENCE: u64 = 600_000;

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
const SIDES: usize = 3;

/// How many times the held client is asked whether it has attached yet, and
/// how long between the asking. Half a minute in all, which is far longer
/// than reaching a host that is already up takes.
const ATTENDING_ATTEMPTS: u32 = 60;

/// How long between those askings.
const ATTENDING_INTERVAL: Duration = Duration::from_millis(500);

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

/// How many hosts the fixture stands up.
const HOSTS: usize = 2;

/// The host the held client watches.
const WATCHED: &str = "host0";

/// The host sessions are made and unmade on.
const CHURNED: &str = "host1";

/// What the held client is called where a container can see it.
const CLIENT_PROCESS: &str = "iznik";

/// And the daemon on a host.
const SERVER_PROCESS: &str = "iznik-server";

/// How much longer than the soak itself its containers may live.
///
/// The clock a soak keeps starts once both hosts are up, and podman's starts
/// when the container does; between them are a bootstrap and, at the other
/// end, the held client being stopped and read. A quarter of an hour is far
/// more than either takes and far less than any soak worth running.
const CONTAINER_MARGIN: Duration = Duration::from_mins(15);

/// How long any one command in a container may take.
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

/// Where the held client writes what it hears.
const TAIL_OUTPUT: &str = "/tmp/iznik-soak-tail.lines";

/// And where the program that compacts it on the way there lives.
const TAIL_FILTER: &str = "/tmp/iznik-soak-compact.awk";

/// What tells that program to read a line at a time rather than a block of
/// them. Without it the first thing the held client says sits unwritten until
/// enough has followed it, and what is waiting for that line is the soak
/// deciding whether the client ever attached at all.
const UNBUFFERED: &str = "-W interactive";

/// How many tenths of a second the held client is given to finish writing
/// after it is interrupted. Ten seconds, which is far longer than flushing a
/// file takes and short enough that a client that will not go is noticed.
const STOP_ATTEMPTS: u32 = 100;

/// What the held client's output is reduced to as it is written.
///
/// A soak of hours moves gigabytes through the pane it holds, and what the
/// byte-loss check needs of each delivery is three numbers: where it starts,
/// how many characters of base 64 carry it, and how many of those are
/// padding. Kept whole, the file would be gigabytes and what could be read
/// back of it would be the end of it; reduced here, a six-hour run fits in a
/// few hundred kilobytes and every line of it is checked.
pub const COMPACT: &str = concat!(
    "{ kind = \"\"; if (match($0, /\"kind\":\"[a-z]+\"/)) ",
    "{ kind = substr($0, RSTART + 8, RLENGTH - 9) } ",
    "at = \"\"; if (match($0, /\"sequence\":[0-9]+/)) ",
    "{ at = substr($0, RSTART + 11, RLENGTH - 11) } ",
    "long = 0; padding = 0; if (match($0, /\"bytes\":\"[^\"]*\"/)) ",
    "{ held = substr($0, RSTART + 9, RLENGTH - 10); long = length(held); ",
    "while (substr(held, length(held), 1) == \"=\") ",
    "{ padding = padding + 1; held = substr(held, 1, length(held) - 1) } } ",
    "print kind, at, long, padding; fflush() }"
);

/// Where its standard error goes, so that a client that refused to start
/// says why rather than saying nothing.
const TAIL_TROUBLE: &str = "/tmp/iznik-soak-tail.err";

/// What a side weighs, read out of `/proc` because the engine image carries
/// no `ps` at all — it is as bare as a machine somebody has just installed,
/// which is the point of it.
///
/// The name is matched whole and the relay is left out. A host runs one
/// daemon and one `--stdio` relay per client connection, and both are called
/// `iznik-server`: a weighing that took the first of them found would be of
/// the daemon at one sample and of a relay at the next, and the series would
/// say a leak and a recovery that neither happened. What is weighed is the
/// daemon — the process that holds the panes and the history, which is where
/// a leak would live — and what is printed is the total of every process that
/// answers to that, so that two of them would be seen rather than one of them
/// picked.
const WEIGH: &str = "for held in /proc/[0-9]*; do \
                     if grep -qsx NAME \"$held/comm\" && \
                     ! grep -qsa -- --stdio \"$held/cmdline\"; then \
                     awk '/VmRSS/{print $2}' \"$held/status\"; fi; done \
                     | awk '{total += $1} END {print total + 0}'";

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
    // A pane, a flood big enough to fill the ring it keeps, and the wait for
    // that flood to be over. Only then does anything start being measured.
    let _opened = step_run(&fixture, &opening_step())?;
    let _settled = step_run(&fixture, &settling_step())?;
    // And the client that holds the pane for the whole soak: the one client
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
        drops: 0,
        churn: 0,
        heard: Heard::default(),
        client: Vec::new(),
        server: Vec::new(),
        churned: Vec::new(),
    };
    let refused = worked(&fixture, &mut report, began);
    // Alive at the end, and asked before it is stopped: a client that died an
    // hour in leaves a stream with no gap in it, which is what a check that
    // only looks for gaps calls whole.
    let living = alive(&fixture)?;
    let _stopped = fixture.exec("engine", &stop_tail(), COMMAND_DEADLINE.0)?;
    report.heard = heard_by(&fixture)?;
    if let Err(source) = judged(&report, &refused, living) {
        return Err(SoakError::Judged {
            report: Box::new(report),
            source: Box::new(source),
        });
    }
    Ok(report)
}

/// Waits for the held client to have attached, or says it never did.
///
/// What it writes first is the pane as it stands, which is what a host sends
/// a client that has just attached; until that line is there, the client is
/// still reaching the host.
///
/// # Errors
///
/// [`SoakError::Lost`] when it never attaches, with whatever it said about
/// why, and [`SoakError::Fixture`] when the container will not say.
fn attended(fixture: &Fixture) -> Result<(), SoakError> {
    for _attempt in 0..ATTENDING_ATTEMPTS {
        let said = fixture.exec("engine", &format!("cat {TAIL_OUTPUT}"), COMMAND_DEADLINE.0)?;
        if !String::from_utf8_lossy(&said.stdout).trim().is_empty() {
            return Ok(());
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

/// Whether the held client is still running.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say.
fn alive(fixture: &Fixture) -> Result<bool, SoakError> {
    let said = fixture.exec("engine", &running(CLIENT_PROCESS), COMMAND_DEADLINE.0)?;
    Ok(String::from_utf8_lossy(&said.stdout).trim() != "0")
}

/// What the held client heard, read back a window of lines at a time.
///
/// A command's output is captured up to a limit and the end of it is what
/// survives, so a file bigger than that read in one go would be checked from
/// its middle and its beginning called whole. The windows are small enough
/// that none of them reaches the limit, and they are read in order, so the
/// reckoning carries from one to the next.
///
/// # Errors
///
/// [`SoakError::Lost`] for a stream with a gap in it or one that says the
/// client stopped, and [`SoakError::Fixture`] when the container will not
/// say.
fn heard_by(fixture: &Fixture) -> Result<Heard, SoakError> {
    let counted = fixture.exec(
        "engine",
        &format!("wc -l < {TAIL_OUTPUT}"),
        COMMAND_DEADLINE.0,
    )?;
    let lines = String::from_utf8_lossy(&counted.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    let mut heard = Heard::default();
    let mut from = 1;
    while from <= lines {
        let to = from.saturating_add(LINES_AT_ONCE).saturating_sub(1);
        let window = fixture.exec(
            "engine",
            &format!("sed -n '{from},{to}p' {TAIL_OUTPUT}"),
            COMMAND_DEADLINE.0,
        )?;
        heard_more(&mut heard, &String::from_utf8_lossy(&window.stdout))
            .map_err(|detail| SoakError::Lost { detail })?;
        from = to.saturating_add(1);
    }
    Ok(heard)
}

/// Whether a soak proved anything, and what it proved.
///
/// Public so that a case can put a report to it directly. What decides
/// whether a six-hour run passed is worth proving without waiting six hours
/// for one.
///
/// # Errors
///
/// [`SoakError::Lost`] when too few rounds finished, when the held client
/// heard nothing or stopped hearing, when a side was never weighed, or when
/// the pane's stream was broken; [`SoakError::Grew`] when a side grew past
/// the ceiling after the warmup.
pub fn judged(report: &Report, refused: &str, living: bool) -> Result<(), SoakError> {
    let lost = |detail: String| SoakError::Lost { detail };
    // A soak in which most rounds did not finish is a soak that proved
    // nothing, however flat the series it took while nothing was happening.
    // Half rather than all of them, because a stack that stopped answering an
    // hour in leaves the rest of the run measuring a corpse.
    if report.drops.saturating_mul(FINISHED_SHARE) < report.rounds {
        return Err(lost(format!(
            "only {} of {} rounds finished; the last to fail said: {refused}",
            report.drops, report.rounds
        )));
    }
    if !living {
        return Err(lost(
            "the held client was gone before the end, so what it heard is not the run".to_owned(),
        ));
    }
    if report.heard.deliveries == 0 {
        return Err(lost(
            "the held client heard nothing, so nothing it heard was whole".to_owned(),
        ));
    }
    // Every byte poured through the pane after the client attached was sent to
    // it, because it returns credit for all of them. Hearing less than was
    // poured is bytes that went missing on the way.
    let poured = poured(FLOOD_LINES).saturating_mul(u64::try_from(report.drops).unwrap_or(0));
    if report.heard.bytes < poured {
        return Err(lost(format!(
            "the held client heard {} bytes of the {poured} its pane was made to say",
            report.heard.bytes
        )));
    }
    // A screen is a host that could not carry a client on from where it was.
    // The pane says nothing while the daemon is stopped, so every one of these
    // cuts is one a resume can be served across; a screen after the first is
    // one that was not.
    if report.heard.screens > 1 {
        return Err(lost(format!(
            "the held client was sent {} screens, so {} reconnections could not be \
             carried on from where it was and the bytes between were lost",
            report.heard.screens,
            report.heard.screens.saturating_sub(1)
        )));
    }
    for (side, samples) in report.weighed() {
        if samples.is_empty() {
            return Err(lost(format!("the {side} was never weighed at all")));
        }
        if let Some(rate) = grown(samples, report.warmup)
            && rate > SOAK_GROWTH_CEILING_PER_HOUR
        {
            return Err(SoakError::Grew {
                side: side.to_owned(),
                rate,
            });
        }
    }
    Ok(())
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
    let (mut refused, mut weigh_at) = (String::new(), Duration::ZERO);
    while began.elapsed() < report.duration {
        // One round: a flood poured through the pane and waited for, the
        // daemon stopped underneath it for longer than any deadline either
        // client keeps, started again, and a host that has to answer
        // afterwards.
        report.rounds = report.rounds.saturating_add(1);
        match round(fixture, report.rounds) {
            Ok(()) => report.drops = report.drops.saturating_add(1),
            Err(refusal) => {
                refused = refusal.to_string();
                writeln!(
                    std::io::stdout(),
                    "round {} did not finish: {refused}",
                    report.rounds
                )
                .unwrap_or_default();
            }
        }
        if fixture
            .exec(
                "engine",
                &tool(&format!("benchmark {CHURNED}")),
                COMMAND_DEADLINE.0,
            )
            .is_ok()
        {
            report.churn = report.churn.saturating_add(1);
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
fn round(fixture: &Fixture, round: usize) -> Result<(), SoakError> {
    let _flooded = step_run(fixture, &flooding_step(round))?;
    let _cut = fixture.exec(WATCHED, &cut(), COMMAND_DEADLINE.0)?;
    let _recovered = step_run(fixture, &recovery_step(round))?;
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

/// A command that says how many processes answer to a name.
fn running(process: &str) -> String {
    format!(
        "found=0; for held in /proc/[0-9]*; do \
         if grep -qsx {process} \"$held/comm\"; then found=$((found + 1)); fi; \
         done; printf '%s\\n' \"$found\""
    )
}

/// Runs one step of the driver in the engine and says what it answered.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the step cannot be written or run, and
/// [`SoakError::Lost`] when the driver refuses it.
fn step_run(fixture: &Fixture, step: &str) -> Result<String, SoakError> {
    let write = format!("cat > {STEP_INPUT} <<'SOAK'\n{step}\nSOAK\n");
    let _written = fixture.exec("engine", &write, COMMAND_DEADLINE.0)?;
    let done = fixture.exec(
        "engine",
        &format!("IZNIK_ARTIFACTS_DIRECTORY={ARTIFACTS} {DRIVER} step < {STEP_INPUT}"),
        COMMAND_DEADLINE.0,
    )?;
    let said = String::from_utf8_lossy(&done.stdout).into_owned();
    if said.contains("\"exit\":0") {
        return Ok(said);
    }
    Err(SoakError::Lost { detail: said })
}

/// The command that starts the held client.
///
/// The one client that lives for the whole soak, and the one that returns
/// credit for everything it takes. What it says goes through the filter above
/// on its way to a file, so that what is read back at the end is the whole
/// run rather than the end of it — and the filter is told to read a line at a
/// time, because the one in these containers otherwise waits for a block of
/// them and a pane that has just been attached to says one line and then goes
/// quiet.
fn tail_command() -> String {
    format!(
        "cat > {TAIL_FILTER} <<'COMPACT'\n{COMPACT}\nCOMPACT\n\
         IZNIK_ARTIFACTS_DIRECTORY={ARTIFACTS} nohup {TOOL} tail {WATCHED} 1 \
         2> {TAIL_TROUBLE} | awk {UNBUFFERED} -f {TAIL_FILTER} > {TAIL_OUTPUT} &"
    )
}

/// And the command that stops it.
///
/// Interrupted, which is the ending it is written to have, and then waited
/// for: what it wrote is only all there once both it and the filter it feeds
/// have gone. Found by name rather than by a process id it wrote down,
/// because what a pipeline answers with is the process id of its last command
/// and the one to interrupt is its first.
fn stop_tail() -> String {
    format!(
        "for held in /proc/[0-9]*; do \
         if grep -qsx {CLIENT_PROCESS} \"$held/comm\"; then \
         kill -INT \"${{held#/proc/}}\"; fi; done; \
         for _ in $(seq 1 {STOP_ATTEMPTS}); do \
         if ! pidof awk {CLIENT_PROCESS} > /dev/null 2>&1; then break; fi; \
         sleep 0.1; done"
    )
}

/// How the link to every client on a host is cut, and made good again.
///
/// The daemon is stopped where it stands and started again in the same
/// command, so a soak that dies between the two leaves nothing frozen behind
/// it. Its process id is read from the lock it holds — the same lock the
/// driver's own fault reads — rather than found by name, because a host runs
/// a relay per connection under that name as well.
///
/// Stopped for [`DROP_SECONDS`], which is longer than the ten seconds a
/// client waits for a pong before it calls a link gone, so that every client
/// on that host has to notice and come back. A cut nobody noticed proves
/// nothing about coming back from one.
fn cut() -> String {
    format!(
        "if [ -n \"$XDG_RUNTIME_DIR\" ]; then lock=\"$XDG_RUNTIME_DIR/iznik/server.lock\"; \
         else lock=\"${{TMPDIR:-/tmp}}/iznik-$(id -u)/server.lock\"; fi; \
         held=$(cat \"$lock\"); kill -STOP \"$held\"; sleep {DROP_SECONDS}; \
         kill -CONT \"$held\""
    )
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

/// What machine this is, as a person would say it.
///
/// The processor and the memory as well as the kernel, because a soak is a
/// measurement of a machine and two machines with the same kernel and the
/// same number of cores are not the same machine. Read from `/proc` inside a
/// container, which shares the host's kernel and sees the host's processor;
/// what a container narrows is how much of it this may use, and the count of
/// cores says that.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the engine will not say.
fn machine(fixture: &Fixture) -> Result<String, SoakError> {
    let said = fixture.exec(
        "engine",
        "printf '%s, %s, %s memory, %s cores' \
         \"$(uname -srm)\" \
         \"$(awk -F: '/model name/{print $2; exit}' /proc/cpuinfo | sed 's/^ *//')\" \
         \"$(awk '/MemTotal/{printf \"%d GiB\", $2 / 1048576}' /proc/meminfo)\" \
         \"$(nproc)\"",
        COMMAND_DEADLINE.0,
    )?;
    Ok(String::from_utf8_lossy(&said.stdout).trim().to_owned())
}

/// What one side weighs now.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say.
fn weighed(
    fixture: &Fixture,
    container: &str,
    process: &str,
    at: Duration,
) -> Result<Sample, SoakError> {
    let said = fixture.exec(
        container,
        &WEIGH.replace("NAME", process),
        COMMAND_DEADLINE.0,
    )?;
    let kibibytes = String::from_utf8_lossy(&said.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    Ok(Sample {
        at,
        bytes: kibibytes.saturating_mul(KIBIBYTE),
    })
}
