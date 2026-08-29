//! What a soak measured, and what reading it says.
//!
//! The series both sides were weighed into, what the held client heard, the
//! growth read out of a series, and the report a person or a note is given.
//! No containers and no clock: everything here is arithmetic over numbers
//! that something else went and got.

use core::time::Duration;
use std::fmt::Write as _;

/// How many numbers have one digit: one to nine, which is where counting the
/// bytes of a flood starts and every decade after it is ten times as many.
const ONE_DIGIT_NUMBERS: u64 = 9;

use crate::soak::{
    BASE64_BYTES, BASE64_CHARACTERS, ENDING, HALVES, SECONDS_PER_HOUR, SECONDS_PER_MINUTE, SIDES,
    SOAK_GROWTH_CEILING_PER_HOUR, TEN,
};

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
    /// How many floods were poured, which is what the held client is held to
    /// hearing: a round that floods and then fails afterwards still made its
    /// pane say every byte of it.
    pub floods: usize,
    /// How many cuts were made and seen to have stopped the daemon.
    pub cuts: usize,
    /// How many of them finished: the link dropped and made good, and the
    /// line typed afterwards heard back.
    pub drops: usize,
    /// How many sessions were made and unmade.
    pub churn: usize,
    /// What the held client heard across every drop.
    pub heard: Heard,
    /// What the held client weighed.
    pub client: Vec<Sample>,
    /// The daemon on the host it watches.
    pub server: Vec<Sample>,
    /// And the daemon on the host where sessions are made and unmade, which
    /// is where a leak in making one would be.
    pub churned: Vec<Sample>,
}

impl Report {
    /// Every series this soak took, with the name it is reported under.
    #[must_use]
    pub fn weighed(&self) -> [(&str, &Vec<Sample>); SIDES] {
        [
            ("held client", &self.client),
            ("daemon it watches", &self.server),
            ("daemon it churns", &self.churned),
        ]
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
    // The middle of an even count is between two samples, and taking the
    // upper of them is taking the larger — which for a run of two samples is
    // taking the maximum, so one peak in the later half would read as a leak
    // and one in the earlier half would hide one.
    let above = sizes.len().checked_div(HALVES)?;
    let below = above.saturating_sub(1);
    let (above, below) = (sizes.get(above)?, sizes.get(below)?);
    if sizes.len().checked_rem(HALVES)? == 1 {
        return Some((at, *above));
    }
    Some((
        at,
        above
            .saturating_add(*below)
            .checked_div(u64::try_from(HALVES).ok()?)?,
    ))
}

/// How many bytes `seq 1 <lines>` says through a pseudoterminal.
///
/// The digits of every number from one to that, and the carriage return and
/// line feed a terminal puts after each of them. Arithmetic rather than a
/// measurement, so that what the held client heard can be held against what
/// its pane was made to say.
#[must_use]
pub fn poured(lines: u64) -> u64 {
    let (mut said, mut digits, mut lowest, mut wide) = (0_u64, 1_u64, 1_u64, ONE_DIGIT_NUMBERS);
    while lowest <= lines && wide > 0 {
        let highest = lowest.saturating_add(wide).saturating_sub(1).min(lines);
        let many = highest.saturating_sub(lowest).saturating_add(1);
        said = said.saturating_add(many.saturating_mul(digits.saturating_add(ENDING)));
        lowest = lowest.saturating_add(wide);
        // A saturating multiplication stops growing rather than wrapping, and
        // a width that stopped growing while `lowest` had saturated too would
        // be a loop that never ends. It ends here instead.
        wide = wide.checked_mul(TEN).unwrap_or(0);
        digits = digits.saturating_add(1);
    }
    said
}

/// What the held client heard, and whether it heard it whole.
#[derive(Clone, Copy, Debug, Default)]
pub struct Heard {
    /// How many deliveries it took.
    pub deliveries: u64,
    /// How many bytes those were.
    pub bytes: u64,
    /// How many times it was sent the pane as it stands rather than carried
    /// on from where it was. One is the attachment; every one after it is a
    /// reconnection the host could not resume.
    pub screens: u64,
    /// The byte the next delivery has to begin at, once one has been read;
    /// nothing after a screen, which moves where the client is.
    pub expected: Option<u64>,
}

/// Reads another window of what the held client printed into what it heard.
///
/// Three things are counted rather than one, because the fourth — that each
/// delivery begins where the one before it ended — is arithmetic the client
/// did itself: the sequence it prints is its own cursor before the bytes are
/// counted, so two deliveries in a row can no more disagree than a number can
/// disagree with itself. It is still checked, because a client whose
/// accounting broke is worth catching, but what a lost byte looks like is a
/// screen: the host could not carry this client on from where it was, and
/// what it sends instead is the truth as it now stands.
///
/// # Errors
///
/// The words for a stream that jumped, one that says the pane was detached —
/// which is the client's own ending, and a stream that ends early is not the
/// run it is reported as — or a line that carries no number where one is
/// owed.
pub fn heard_more(heard: &mut Heard, said: &str) -> Result<(), String> {
    for line in said.lines() {
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else {
            continue;
        };
        if kind == "detached" {
            return Err("the pane was detached, so the held client stopped listening".to_owned());
        }
        let (Some(at), Some(long), Some(padding)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(at), Ok(long), Ok(padding)) = (
            at.parse::<u64>(),
            long.parse::<u64>(),
            padding.parse::<u64>(),
        ) else {
            return Err(format!("a delivery said a number that is not one: {line}"));
        };
        if kind == "screen" {
            heard.screens = heard.screens.saturating_add(1);
            heard.expected = None;
            continue;
        }
        if kind != "output" {
            continue;
        }
        if let Some(wanted) = heard.expected
            && wanted != at
        {
            return Err(format!(
                "the pane's stream jumped from {wanted} to {at}, so this client's own \
                 accounting of what it has taken is broken"
            ));
        }
        let long = decoded_length(long, padding);
        heard.deliveries = heard.deliveries.saturating_add(1);
        heard.bytes = heard.bytes.saturating_add(long);
        heard.expected = Some(at.saturating_add(long));
    }
    Ok(())
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

/// A duration as the whole minutes a report says.
#[must_use]
pub fn minutes(held: Duration) -> u64 {
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
         - **Cuts:** {}, each seen to have stopped the daemon it named\n\
         - **Pane churn:** {} sessions made and unmade\n\
         - **Held client:** {} deliveries, {} bytes, {} screens\n\
         - **Growth ceiling:** {SOAK_GROWTH_CEILING_PER_HOUR} bytes an hour, after the warmup\n",
        report.machine,
        minutes(report.duration),
        minutes(report.warmup),
        report.rounds,
        report.drops,
        report.cuts,
        report.churn,
        report.heard.deliveries,
        report.heard.bytes,
        report.heard.screens
    );
    for (side, samples) in report.weighed() {
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
