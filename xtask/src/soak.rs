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
//! What it fails on: memory growing past a ceiling after the warmup, or a
//! reconnection losing a byte. The first is the leak; the second is the resume
//! that plan 0005 exists for, asked again after hours rather than after a
//! second.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter, Write as _};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iznik_harness::fixture::{Fixture, FixtureError, FixtureOptions};
use iznik_harness::process::Deadline;
use iznik_harness::staging::{STAGING_DEADLINE, stage};

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

/// How long any one thing a round waits for may take. Generous, because a
/// round shares a machine with whatever else is building on it, and a wait
/// that is merely slow is not one that failed.
const PATIENCE: u64 = 120_000;

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

/// How a daemon that a round left stopped is started again.
///
/// A round pauses the host and resumes it, and a round that failed in between
/// left it paused; every round after that would be of a host that is not
/// running. Continuing a process that was never stopped is nothing, so it
/// costs nothing to do after any failure at all.
const REVIVE: &str = "for held in /proc/[0-9]*; do \
                      if grep -qsx iznik-server \"$held/comm\"; then \
                      kill -CONT \"${held#/proc/}\"; fi; done";

/// What both sides weigh, read out of `/proc` rather than from `ps`, which in
/// these containers is a busybox that has neither the flag nor the column.
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

/// One weighing of one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// How far into the soak it was taken.
    pub at: Duration,
    /// What was weighed, in bytes.
    pub bytes: u64,
}

/// What a soak found.
#[derive(Clone, Debug)]
pub struct Report {
    /// What machine it ran on.
    pub machine: String,
    /// How long it ran.
    pub duration: Duration,
    /// How much of that was not measured.
    pub warmup: Duration,
    /// How many rounds were attempted.
    pub rounds: usize,
    /// How many of them finished: the link dropped and made good, and the
    /// line typed afterwards heard back.
    pub drops: usize,
    /// How many sessions were made and unmade.
    pub churn: usize,
    /// What the held client heard across every drop.
    pub heard: Heard,
    /// What the client weighed.
    pub client: Vec<Sample>,
    /// And the server.
    pub server: Vec<Sample>,
}

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
    /// A reconnection lost a byte, or the client could not be held.
    Lost {
        /// What the driver said.
        detail: String,
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

/// How much a series grew an hour after the warmup, when it grew at all.
///
/// The measured samples are cut in half and the middle of the first half is
/// held against the middle of the second: what a leak does is move where a
/// process sits, and what a flood does is put a peak on top of it. Two points
/// would be at the mercy of which of the two each of them landed on — a run
/// whose last sample fell on a peak and whose first fell in a trough would
/// report megabytes an hour of a process that never grew — and a middle is
/// neither.
///
/// Nothing before the warmup is looked at, because a process that is still
/// growing into its work is not leaking, and a series with fewer than two
/// samples after it says nothing rather than guessing.
#[must_use]
pub fn grown(samples: &[Sample], warmup: Duration) -> Option<u64> {
    let measured: Vec<&Sample> = samples.iter().filter(|held| held.at >= warmup).collect();
    let (earlier, later) = measured.split_at_checked(measured.len().checked_div(HALVES)?)?;
    let (earlier, later) = (middle(earlier)?, middle(later)?);
    let over = later.0.saturating_sub(earlier.0).as_secs();
    if over == 0 {
        return None;
    }
    let grew = later.1.saturating_sub(earlier.1);
    grew.saturating_mul(SECONDS_PER_HOUR).checked_div(over)
}

/// When a run of samples was taken, and what a process sat at over it.
///
/// The middle of the times because they are evenly spaced, and the middle of
/// the sizes because they are not: a sorted middle is a size the process
/// really was, unmoved by the one sample that landed while a flood was in
/// flight.
fn middle(samples: &[&Sample]) -> Option<(Duration, u64)> {
    let first = samples.first()?;
    let last = samples.last()?;
    let at = first
        .at
        .saturating_add(last.at)
        .checked_div(HALVES.try_into().ok()?)?;
    let mut sizes: Vec<u64> = samples.iter().map(|held| held.bytes).collect();
    sizes.sort_unstable();
    let size = sizes.get(sizes.len().checked_div(HALVES)?)?;
    Some((at, *size))
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
    match soaked(asked.0, asked.1) {
        Ok(report) => {
            writeln!(std::io::stdout(), "{}", rendered(&report)).unwrap_or_default();
            ExitCode::SUCCESS
        }
        Err(refusal) => {
            writeln!(std::io::stderr(), "{refusal}").unwrap_or_default();
            ExitCode::FAILURE
        }
    }
}

/// The duration and warmup a command line asks for.
///
/// # Errors
///
/// [`SoakError::Usage`] for a flag this does not know or a number it cannot
/// read.
fn asked_for(arguments: &[OsString]) -> Result<(Duration, Duration), SoakError> {
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
    // asks for its own length and the margin the standing up and the taking
    // down at either end need.
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
    // A pane to hold open, and the client that holds it. The tail is what is
    // weighed: it is the one client that lives for the whole soak, and it
    // returns credit for everything it takes — which a client that watched a
    // flood without crediting could not.
    let _opened = step_run(&fixture, &opening_step())?;
    let _started = fixture.exec("engine", &tail_command(), COMMAND_DEADLINE.0)?;
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
    };
    let refused = worked(&fixture, &mut report, began)?;
    // A soak in which most rounds did not finish is a soak that proved
    // nothing, however flat the two series it took while nothing was
    // happening. Half rather than all of them, because a stack that stopped
    // answering an hour in leaves the rest of the run measuring a corpse, and
    // a series taken from one is exactly as flat as a series with no leak in
    // it.
    if report.drops.saturating_mul(FINISHED_SHARE) < report.rounds {
        return Err(SoakError::Lost {
            detail: format!(
                "only {} of {} rounds finished; the last to fail said: {refused}",
                report.drops, report.rounds
            ),
        });
    }
    // What the held client saw, from beginning to end: every byte it was sent
    // in the order it was sent, across every drop.
    let _stopped = fixture.exec("engine", &stop_tail(), COMMAND_DEADLINE.0)?;
    let said = fixture.exec("engine", &format!("cat {TAIL_OUTPUT}"), COMMAND_DEADLINE.0)?;
    let printed = String::from_utf8_lossy(&said.stdout).into_owned();
    report.heard = heard_in(&printed).map_err(|detail| SoakError::Lost { detail })?;
    if report.heard.deliveries == 0 {
        return Err(SoakError::Lost {
            detail: "the held client heard nothing, so nothing it heard was whole".to_owned(),
        });
    }
    if report.client.is_empty() {
        return Err(SoakError::Lost {
            detail: "the held client was never weighed, so it was never held".to_owned(),
        });
    }
    for (side, samples) in [("client", &report.client), ("server", &report.server)] {
        if let Some(rate) = grown(samples, warmup)
            && rate > SOAK_GROWTH_CEILING_PER_HOUR
        {
            return Err(SoakError::Grew {
                side: side.to_owned(),
                rate,
            });
        }
    }
    Ok(report)
}

/// Runs rounds back to back until the clock says stop, weighing both sides on
/// a schedule of its own.
///
/// The work has no schedule and the sampling does: a leak is found by working
/// and not by waiting, and a series is only a series if its points are evenly
/// spaced. Answers with what the last round to fail said, for the judgement
/// the caller makes about how many of them did.
///
/// # Errors
///
/// [`SoakError::Fixture`] when a container will not say what a side weighs.
fn worked(fixture: &Fixture, report: &mut Report, began: Instant) -> Result<String, SoakError> {
    let interval = interval_for(report.duration);
    let (mut refused, mut weigh_at) = (String::new(), Duration::ZERO);
    while began.elapsed() < report.duration {
        // One round: a flood through the pane the tail is watching, the link
        // dropped and made good, and a line typed afterwards that has to come
        // back. Each round is its own client with its own window, which is
        // what lets the flood be one. They run back to back for the whole
        // soak — the sampling has a schedule, the work does not, because a
        // leak is found by working and not by waiting.
        report.rounds = report.rounds.saturating_add(1);
        match step_run(fixture, &round_step(report.rounds)) {
            Ok(_finished) => report.drops = report.drops.saturating_add(1),
            Err(refusal) => {
                refused = refusal.to_string();
                writeln!(
                    std::io::stdout(),
                    "round {} did not finish: {refused}",
                    report.rounds
                )
                .unwrap_or_default();
                // A round that stopped between the pause and the resume left
                // the daemon stopped, and everything after it would be a soak
                // of a host that is not running. Continuing a process that was
                // never stopped is nothing, so this is safe after any failure
                // at all — and a revival that cannot be run is not worth
                // ending hours of measurement for, because what a stack that
                // has stopped answering looks like is rounds that do not
                // finish, which is counted below.
                let _revived = fixture.exec(WATCHED, REVIVE, COMMAND_DEADLINE.0);
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
        let weighing = [
            (&mut report.client, "engine", CLIENT_PROCESS),
            (&mut report.server, WATCHED, SERVER_PROCESS),
        ];
        for (series, container, process) in weighing {
            let sample = weighed(fixture, container, process, at)?;
            if sample.bytes > 0 {
                series.push(sample);
            }
            writeln!(
                std::io::stdout(),
                "sample at {}s: {} bytes ({process})",
                at.as_secs(),
                sample.bytes
            )
            .unwrap_or_default();
        }
    }
    Ok(refused)
}

/// What the held client heard, and whether it heard it whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Heard {
    /// How many deliveries it took.
    pub deliveries: u64,
    /// How many bytes those were.
    pub bytes: u64,
}

/// What the held client heard, or where the pane's stream stopped being one.
///
/// The held client prints every delivery with the byte it starts at, so the
/// stream is whole when each one begins where the one before it ended. A
/// screen is where the host gave up on catching a client up and sent the
/// truth instead; it moves the cursor legitimately, so it starts the
/// reckoning again rather than breaking it.
///
/// Counted as well as checked, because a stream with no gap in it and a
/// stream that was never there look the same to a check that only looks for
/// gaps.
///
/// # Errors
///
/// The gap, in words, when one delivery does not begin where the one before
/// it ended.
pub fn heard_in(said: &str) -> Result<Heard, String> {
    let mut heard = Heard::default();
    let mut expected: Option<u64> = None;
    for line in said.lines() {
        let mut fields = line.split_whitespace();
        let (Some(kind), Some(at), Some(long), Some(padding)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if kind == "screen" {
            expected = None;
            continue;
        }
        if kind != "output" {
            continue;
        }
        let (Ok(at), Ok(long), Ok(padding)) = (
            at.parse::<u64>(),
            long.parse::<u64>(),
            padding.parse::<u64>(),
        ) else {
            return Err(format!("a delivery said a number that is not one: {line}"));
        };
        if let Some(wanted) = expected
            && wanted != at
        {
            return Err(format!(
                "the pane's stream jumped from {wanted} to {at}, so a reconnection lost bytes"
            ));
        }
        let long = decoded_length(long, padding);
        heard.deliveries = heard.deliveries.saturating_add(1);
        heard.bytes = heard.bytes.saturating_add(long);
        expected = Some(at.saturating_add(long));
    }
    Ok(heard)
}

/// How many bytes some base 64 stands for, from how many characters carry it
/// and how many of those are padding.
fn decoded_length(characters: u64, padding: u64) -> u64 {
    characters
        .saturating_mul(BASE64_BYTES)
        .checked_div(BASE64_CHARACTERS)
        .unwrap_or(0)
        .saturating_sub(padding)
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
/// run rather than the end of it.
fn tail_command() -> String {
    format!(
        "cat > {TAIL_FILTER} <<'COMPACT'\n{COMPACT}\nCOMPACT\n\
         IZNIK_ARTIFACTS_DIRECTORY={ARTIFACTS} nohup {TOOL} tail {WATCHED} 1 \
         2> {TAIL_TROUBLE} | awk -f {TAIL_FILTER} > {TAIL_OUTPUT} &"
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
/// # Errors
///
/// [`SoakError::Fixture`] when the engine will not say.
fn machine(fixture: &Fixture) -> Result<String, SoakError> {
    let said = fixture.exec(
        "engine",
        "printf '%s, %s cores' \"$(uname -srm)\" \"$(nproc)\"",
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

/// The step that makes the pane the soak holds open.
fn opening_step() -> String {
    step_of(
        "opening",
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"create_session\", alias = \"{WATCHED}\", name = \"soak\" }},\n  \
             {{ kind = \"await_delta\", alias = \"{WATCHED}\", generation = 1 }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"seq 1 {OPENING_FLOOD_LINES}\\n\" }},\n"
        ),
    )
}

/// One round: a flood, a drop, and a line that has to come back after it.
///
/// It subscribes once and never again. What proves the drop was survived is
/// that the line typed after it comes back on the subscription made before
/// it: the client re-establishes what it held when the link returns, and a
/// round that subscribed a second time would be proving that a fresh
/// subscription works rather than that an old one lived.
///
/// The flood is waited for rather than only typed — a shell runs what it is
/// given in the order it is given, so a line echoed after it comes back only
/// once the flood has been produced — and it is sized to the window this
/// client is given. A step of the driver returns no credit, so a flood past
/// that window would stall against a host that is behaving exactly as it
/// should. The flood that is bigger than any window is the one poured before
/// the soak begins, which the held client drains and pays for.
///
/// The waiting either side of the drop is not politeness: a keystroke handed
/// to a host that has nowhere to send it is dropped on purpose, so a round
/// that typed before the link was back would be asking for something nobody
/// took.
///
/// Numbered by the attempt rather than by the success, so that no two rounds
/// of one soak wait for the same line.
fn round_step(round: usize) -> String {
    step_of(
        "round",
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"subscribe\", alias = \"{WATCHED}\", pane = 1 }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"seq 1 {FLOOD_LINES}\\n\" }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"echo flooded-{round}\\n\" }},\n  \
             {{ kind = \"await_bytes\", alias = \"{WATCHED}\", pane = 1, \
             contains = \"flooded-{round}\", within_milliseconds = {PATIENCE} }},\n  \
             {{ kind = \"pause_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"reconnecting\" }},\n  \
             {{ kind = \"resume_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"echo round-{round}\\n\" }},\n  \
             {{ kind = \"await_bytes\", alias = \"{WATCHED}\", pane = 1, \
             contains = \"round-{round}\", within_milliseconds = {PATIENCE} }},\n"
        ),
    )
}

/// One step of the driver, around a list of actions.
fn step_of(id: &str, actions: &str) -> String {
    format!(
        "scenario = \"soak\"\nid = \"{id}\"\ncontainer = \"engine\"\n\
         timeout_seconds = {}\n\n[manager]\npatience_milliseconds = {}\n\
         pong_deadline_milliseconds = 4000\nping_interval_milliseconds = 500\n\
         backoff_initial_milliseconds = 200\nbackoff_maximum_milliseconds = 4000\n\
         actions = [\n{actions}]\n",
        STEP_DEADLINE.as_secs(),
        STEP_DEADLINE.as_millis()
    )
}

/// A duration as the whole minutes a report says.
fn minutes(held: Duration) -> u64 {
    held.as_secs().checked_div(SECONDS_PER_MINUTE).unwrap_or(0)
}

/// The report as the note this task commits.
#[must_use]
pub fn rendered(report: &Report) -> String {
    let mut said = String::new();
    let _head = writeln!(
        said,
        "# Soak report\n\n\
         - **Machine:** {}\n\
         - **Duration:** {} minutes\n\
         - **Warmup:** {} minutes\n\
         - **Rounds:** {} attempted, {} finished\n\
         - **Pane churn:** {} sessions made and unmade\n\
         - **Held client:** {} deliveries, {} bytes, whole across every drop\n\
         - **Growth ceiling:** {SOAK_GROWTH_CEILING_PER_HOUR} bytes an hour, after the warmup\n",
        report.machine,
        minutes(report.duration),
        minutes(report.warmup),
        report.rounds,
        report.drops,
        report.churn,
        report.heard.deliveries,
        report.heard.bytes
    );
    for (side, samples) in [("client", &report.client), ("server", &report.server)] {
        let _series = writeln!(
            said,
            "\n## The {side}, in bytes\n\n| At | Resident |\n|---|---|"
        );
        for sample in samples {
            let _row = writeln!(said, "| {}s | {} |", sample.at.as_secs(), sample.bytes);
        }
        let grew = grown(samples, report.warmup).map_or_else(
            || "nothing measured after the warmup".to_owned(),
            |rate| format!("{rate} bytes an hour"),
        );
        let _grew = writeln!(said, "\nGrowth after the warmup: {grew}.");
    }
    said
}
