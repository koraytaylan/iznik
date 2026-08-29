//! The soak, run for a minute.
//!
//! What the soak is for cannot be proven in a minute — a leak of a few
//! kilobytes an hour needs hours to show — so what is proven here is that the
//! thing runs: two hosts stood up, a client held open across drops that loses
//! no byte, sessions made and unmade beside it, and both sides weighed. The
//! six-hour run is a person's, and the release checklist is where they are
//! asked for it.
//!
//! The growth check is separate and needs no containers at all: it is
//! arithmetic over a series of numbers, and it is asked directly about a
//! series that climbs and one that levels off.

use core::time::Duration;
use std::path::{Path, PathBuf};

use xtask::soak::{
    COMPACT, FLOOD_LINES, Heard, OPENING_FLOOD_LINES, Report, SERVER_PROCESS, SIDES,
    SOAK_GROWTH_CEILING_PER_HOUR, Sample, SoakError, WEIGH, asked_for, grown, heard_more, judged,
    poured, rendered, soaked,
};

/// How long the case runs the whole stack for.
const MINUTE: Duration = Duration::from_mins(1);

/// And how much of it is not measured. None, for a series of numbers this
/// case makes up.
const NO_WARMUP: Duration = Duration::ZERO;

/// How much of the minute is not measured: all of it. A stack that has just
/// been stood up is filling the ring every pane keeps and the buffers every
/// process grows into, and a minute is too short for any of that to be over.
/// What this case asks is whether the soak runs — the growth it is for is
/// asked of a series directly, below, and of the six-hour run by a person.
const WHOLE_WARMUP: Duration = MINUTE;

/// How long a synthetic series runs for.
const SERIES: u64 = 3600;

/// How often a synthetic series is sampled.
const EVERY: u64 = 60;

/// The report this task commits, relative to the repository root.
const NOTE: &str = "docs/notes/soak.md";

/// The checklist a release runs through, beside it.
const CHECKLIST: &str = "docs/notes/release-checklist.md";

/// The scenario that proves the soak's census in a container.
const SCENARIO: &str = "regression/scenarios/soak-and-release/short-soak.toml";

/// What the census names the process it weighs, before a name is put in.
const NAME: &str = "NAME";

/// The step of that scenario which weighs.
const WEIGHING_STEP: &str = "weigh-the-daemon";

/// How long the committed report is of, in minutes.
const REPORTED_MINUTES: u64 = 10;

/// What a synthetic series starts at.
const RESIDENT: u64 = 32 * 1024 * 1024;

/// The repository root, which is where the crate this tests lives under.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// A series that grows by `per_hour` bytes an hour, sampled every minute.
fn climbing(per_hour: u64) -> Vec<Sample> {
    (0..=SERIES.checked_div(EVERY).unwrap_or(1))
        .map(|at| {
            let seconds = at.saturating_mul(EVERY);
            let grown = per_hour
                .saturating_mul(seconds)
                .checked_div(SERIES)
                .unwrap_or(0);
            Sample {
                at: Duration::from_secs(seconds),
                bytes: RESIDENT.saturating_add(grown),
            }
        })
        .collect()
}

/// # Panics
///
/// When a series that climbs past the ceiling is not caught, or one that
/// levels off is.
#[test]
fn regression_soak_notices_a_series_that_climbs() {
    let steep = SOAK_GROWTH_CEILING_PER_HOUR.saturating_mul(4);
    let rate = grown(&climbing(steep), NO_WARMUP);
    assert!(
        rate.is_some_and(|held| held > SOAK_GROWTH_CEILING_PER_HOUR),
        "a series climbing {steep} bytes an hour is over the ceiling: {rate:?}"
    );
    // One that levels off is not, however high it started.
    let level = grown(&climbing(0), NO_WARMUP);
    assert_eq!(
        level,
        Some(0),
        "and one that levels off grew by nothing: {level:?}"
    );
    // And one just under it passes, so the ceiling is a ceiling and not a
    // ban on growth.
    let gentle = SOAK_GROWTH_CEILING_PER_HOUR.saturating_div(2);
    let slow = grown(&climbing(gentle), NO_WARMUP);
    assert!(
        slow.is_some_and(|held| held <= SOAK_GROWTH_CEILING_PER_HOUR),
        "a series climbing {gentle} bytes an hour is under it: {slow:?}"
    );
}

/// # Panics
///
/// When what is before the warmup is measured, or a series with nothing after
/// it is guessed at.
#[test]
fn regression_soak_measures_nothing_before_the_warmup() {
    let steep = SOAK_GROWTH_CEILING_PER_HOUR.saturating_mul(8);
    let mut samples = climbing(steep);
    // Everything this series does, it does before the warmup ends.
    let warmup = Duration::from_secs(SERIES);
    assert_eq!(
        grown(&samples, warmup),
        None,
        "a series with one sample after the warmup says nothing rather than guessing"
    );
    // With something after it, what is measured is only what came after.
    samples.push(Sample {
        at: Duration::from_secs(SERIES.saturating_mul(2)),
        bytes: RESIDENT.saturating_add(steep),
    });
    assert_eq!(
        grown(&samples, warmup),
        Some(0),
        "and what grew before it is not counted against what came after"
    );
}

/// # Panics
///
/// When the soak does not run, or runs without doing what it is for.
#[test]
#[ignore = "stands two hosts up in containers for a minute"]
fn regression_soak_runs_the_whole_stack_for_a_minute() {
    let report = soaked(MINUTE, WHOLE_WARMUP).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        !report.machine.is_empty(),
        "the report says what machine it ran on"
    );
    assert!(report.drops > 0, "the link was dropped and made good again");
    assert!(
        report.churn > 0,
        "and sessions were made and unmade beside it"
    );
    assert!(
        !report.client.is_empty() && !report.server.is_empty(),
        "and both sides were weighed at least once"
    );
    // A weighing of nothing is a weighing that failed: a process that is
    // running has a resident size.
    for sample in report.client.iter().chain(report.server.iter()) {
        assert!(sample.bytes > 0, "and what was weighed weighed something");
    }
    // A stream with no gap in it and a stream that was never there look the
    // same to a check that only looks for gaps, so the check says how much it
    // read as well as whether it was whole.
    assert!(
        report.heard.deliveries > 0 && report.heard.bytes > 0,
        "the held client heard something to be whole about: {:?}",
        report.heard
    );
    let said = rendered(&report);
    for named in [
        "Machine",
        "Duration",
        "Rounds",
        "Cuts",
        "Pane churn",
        "Held client",
        "## The held client, in bytes",
        "## The daemon it watches, in bytes",
        "## The daemon it churns, in bytes",
    ] {
        assert!(said.contains(named), "the report carries {named}: {said}");
    }
}

/// # Panics
///
/// When a screen is not counted, a detached pane is not an ending, or a
/// client's own accounting going wrong is not caught.
#[test]
fn regression_soak_counts_what_the_held_client_was_sent() {
    // Two deliveries of four bytes each, the second beginning where the first
    // ended.
    let mut heard = Heard::default();
    heard_more(&mut heard, "output 0 8 2\noutput 4 8 2\n").unwrap_or_else(|gap| panic!("{gap}"));
    assert_eq!(
        (heard.deliveries, heard.bytes, heard.screens),
        (2, 8, 0),
        "what it heard is counted: {heard:?}"
    );
    // A screen is the host failing to carry this client on from where it was,
    // which is what a lost byte looks like from here. It is counted, and it
    // begins the reckoning again rather than breaking it.
    let mut redrawn = Heard::default();
    heard_more(
        &mut redrawn,
        "output 0 8 2\nscreen 99 4 2\noutput 100 8 2\n",
    )
    .unwrap_or_else(|gap| panic!("{gap}"));
    assert_eq!(
        (redrawn.deliveries, redrawn.screens),
        (2, 1),
        "a screen is counted and not called a gap: {redrawn:?}"
    );
    // A detached pane is the client's own ending, and a stream that ends
    // early is not the run it would otherwise be reported as.
    let mut detached = Heard::default();
    assert!(
        heard_more(&mut detached, "output 0 8 2\ndetached 0 0\n").is_err(),
        "a detached pane ends the run rather than passing quietly"
    );
    // And the client's own accounting going wrong is still caught, though it
    // is not what a lost byte looks like: the sequence it prints is its own
    // cursor, so two deliveries in a row cannot disagree unless it is broken.
    let mut broken = Heard::default();
    assert!(
        heard_more(&mut broken, "output 0 8 2\noutput 5 8 2\n").is_err(),
        "a delivery that does not begin where the one before it ended is a broken client"
    );
    // Nothing at all is nothing heard, not a stream that was whole.
    let mut nothing = Heard::default();
    heard_more(&mut nothing, "").unwrap_or_else(|gap| panic!("{gap}"));
    assert_eq!(nothing.deliveries, 0, "nothing heard is nothing heard");
    // What is read a window at a time is read as one stream: the second
    // window continues the first.
    let mut across = Heard::default();
    heard_more(&mut across, "output 0 8 2\n").unwrap_or_else(|gap| panic!("{gap}"));
    assert!(
        heard_more(&mut across, "output 5 8 2\n").is_err(),
        "and the reckoning carries from one window to the next"
    );
}

/// # Panics
///
/// When what a pane is made to say is not what the arithmetic says it is.
#[test]
fn regression_soak_counts_what_a_flood_says() {
    // Nine one-digit numbers, each with a carriage return and a line feed.
    assert_eq!(poured(9), 27, "one digit and an ending, nine times");
    // And the ninety two-digit ones after them.
    assert_eq!(poured(99), 27 + 360, "then two digits, ninety times");
    // The flood a round pours, which is what the held client is held to.
    assert_eq!(
        poured(30_000),
        198_894,
        "thirty thousand lines is a little under two hundred kilobytes"
    );
    assert_eq!(poured(0), 0, "and nothing said is nothing counted");
}

/// # Panics
///
/// When what the held client prints is not what the check reads, or when
/// `awk` will not run.
#[test]
fn regression_soak_compacts_what_the_held_client_prints() {
    // Exactly what `iznik tail` writes, which is where the numbers the check
    // reads come from. The filter runs between the two in every soak, and a
    // check proven only against lines this case made up would be proven
    // against a format nothing produces.
    let printed = "{\"kind\":\"screen\",\"pane\":1,\"sequence\":0,\"columns\":80,\
                   \"rows\":24,\"bytes\":\"eA==\",\"encoding\":\"base64\"}\n\
                   {\"kind\":\"output\",\"pane\":1,\"sequence\":1,\
                   \"bytes\":\"YWJjZA==\",\"encoding\":\"base64\"}\n\
                   {\"kind\":\"output\",\"pane\":1,\"sequence\":5,\
                   \"bytes\":\"ZWZnaGlqa2w=\",\"encoding\":\"base64\"}\n";
    let reduced = compacted(printed).unwrap_or_else(|error| panic!("{error}"));
    let mut heard = Heard::default();
    heard_more(&mut heard, &reduced).unwrap_or_else(|gap| panic!("{gap}"));
    // Four bytes then eight, the second beginning where the first ended, and
    // the screen before them counted rather than counted as a delivery.
    assert_eq!(
        (heard.deliveries, heard.bytes, heard.screens),
        (2, 12, 1),
        "what the filter wrote is what the check reads: {heard:?}"
    );
    // The line the client prints as it stops — which carries neither a
    // sequence nor any bytes — survives the filter as something the check can
    // still recognise as an ending.
    let ending =
        compacted("{\"kind\":\"detached\",\"pane\":1}\n").unwrap_or_else(|error| panic!("{error}"));
    let mut stopped = Heard::default();
    assert!(
        heard_more(&mut stopped, &ending).is_err(),
        "the filter keeps enough of a detached pane for the check to end on it: {ending:?}"
    );
    // A line whose kind the filter does not recognise still comes out with
    // four fields, so it is skipped as the one thing it is rather than read
    // as a delivery whose numbers have all moved one place left.
    let strange = compacted(
        "{\"kind\":\"Something_New\",\"pane\":1,\"sequence\":700,\
                             \"bytes\":\"YWJjZGVmZ2g=\"}\n",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        strange.split_whitespace().count(),
        4,
        "every line the filter writes has four fields: {strange:?}"
    );
    // And a delivery that does not follow the one before it is still caught
    // after passing through the filter.
    let jumped = printed.replace("\"sequence\":5", "\"sequence\":6");
    let broken = compacted(&jumped).unwrap_or_else(|error| panic!("{error}"));
    let mut jumping = Heard::default();
    assert!(
        heard_more(&mut jumping, &broken).is_err(),
        "a gap survives the filter, or the filter is hiding one"
    );
}

/// What the filter every soak runs makes of what the held client printed.
///
/// # Errors
///
/// What went wrong when `awk` cannot be run, will not take the program, or
/// the files it needs cannot be written.
fn compacted(printed: &str) -> Result<String, String> {
    let directory = std::env::temp_dir().join(format!("iznik-soak-compact-{}", std::process::id()));
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let program = directory.join("compact.awk");
    std::fs::write(&program, format!("{COMPACT}\n")).map_err(|error| error.to_string())?;
    let said = directory.join("printed.jsonl");
    std::fs::write(&said, printed).map_err(|error| error.to_string())?;
    let done = std::process::Command::new("awk")
        .arg("-f")
        .arg(&program)
        .arg(&said)
        .output()
        .map_err(|error| format!("awk: {error}"));
    // Whatever happened, this leaves nothing behind: /tmp on a machine that
    // runs these all day is a finite number of inodes, and every other case
    // in this directory clears up after itself.
    let _swept = std::fs::remove_dir_all(&directory);
    let done = done?;
    if !done.status.success() {
        return Err(format!(
            "awk would not take the program: {}",
            String::from_utf8_lossy(&done.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

/// # Panics
///
/// When the committed report is not of a real soak of at least the length the
/// task asks for.
#[test]
fn regression_soak_committed_a_report_of_ten_minutes() {
    let said = std::fs::read_to_string(root().join(NOTE))
        .unwrap_or_else(|error| panic!("{NOTE}: {error}"));
    for named in [
        "Machine",
        "Duration",
        "Warmup",
        "Rounds",
        "Cuts",
        "Pane churn",
        "Held client",
        "## The held client, in bytes",
        "## The daemon it watches, in bytes",
        "## The daemon it churns, in bytes",
    ] {
        assert!(said.contains(named), "the committed report says {named}");
    }
    let minutes = said
        .lines()
        .find_map(|line| line.strip_prefix("- **Duration:** "))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|held| held.parse::<u64>().ok());
    assert!(
        minutes.is_some_and(|held| held >= REPORTED_MINUTES),
        "and it is of a soak of at least {REPORTED_MINUTES} minutes: {minutes:?}"
    );
}

/// # Panics
///
/// When the checklist does not name what a release runs through, in the order
/// it runs through it.
#[test]
fn regression_soak_checklist_names_a_release_in_order() {
    let said = std::fs::read_to_string(root().join(CHECKLIST))
        .unwrap_or_else(|error| panic!("{CHECKLIST}: {error}"));
    let mut over = 0;
    for named in [
        "docs/notes/soak.md",
        "docs/notes/baseline.md",
        "cargo xtask check",
        "xtask claims coverage",
        "cargo xtask distribution",
        "include/iznik.h",
    ] {
        let at = said
            .find(named)
            .unwrap_or_else(|| panic!("the checklist names {named}"));
        assert!(
            at > over,
            "and names {named} after what comes before it, at {at} rather than after {over}"
        );
        over = at;
    }
}

/// How many rounds the report a case spoils stands for.
const ROUNDS: usize = 10;

/// A report of a run that passed, which a case then spoils one way at a time.
fn passing() -> Report {
    let level = climbing(0);
    Report {
        machine: "a machine".to_owned(),
        duration: Duration::from_mins(10),
        warmup: NO_WARMUP,
        rounds: ROUNDS,
        floods: ROUNDS,
        cuts: ROUNDS,
        streak: 0,
        drops: ROUNDS,
        churn: ROUNDS,
        heard: Heard {
            deliveries: 100,
            bytes: poured(FLOOD_LINES).saturating_mul(u64::try_from(ROUNDS).unwrap_or(0)),
            screens: 1,
            expected: None,
            attached: Some(poured(OPENING_FLOOD_LINES)),
        },
        client: level.clone(),
        server: level.clone(),
        churned: level,
    }
}

/// # Panics
///
/// When a report that should pass does not, or when spoiling one thing about
/// what the run did is not caught.
#[test]
fn regression_soak_judges_a_run_by_what_it_did() {
    let passes = passing();
    assert!(
        judged(&passes, "", true).is_ok(),
        "a run that did everything asked of it passes"
    );
    // A held client that was gone before the end.
    assert!(
        judged(&passes, "", false).is_err(),
        "a client that died leaves a stream that is not the run"
    );
    // One that heard less than its pane was made to say: bytes went missing.
    let mut quiet = passing();
    quiet.heard.bytes = quiet.heard.bytes.saturating_sub(1);
    assert!(
        judged(&quiet, "", true).is_err(),
        "hearing less than was poured is bytes that went missing"
    );
    // Held against the floods that were poured and not the rounds that
    // finished, so a round that flooded and then failed buys no slack.
    let mut halfway = passing();
    halfway.drops = ROUNDS.saturating_div(2);
    halfway.heard.bytes =
        poured(FLOOD_LINES).saturating_mul(u64::try_from(halfway.drops).unwrap_or(0));
    assert!(
        judged(&halfway, "", true).is_err(),
        "a round that failed after its flood does not excuse the flood it poured"
    );
    // A client that heard nothing at all.
    let mut unheard = passing();
    unheard.heard.deliveries = 0;
    assert!(
        judged(&unheard, "", true).is_err(),
        "a client that heard nothing heard nothing whole"
    );
    // A client that attached to a pane which had said nothing: the flood
    // poured before the clock starts never happened, and the ring it fills
    // would be weighed filling itself.
    let mut empty = passing();
    empty.heard.attached = Some(0);
    assert!(
        judged(&empty, "", true).is_err(),
        "a client that attached to an empty pane is a flood that never was"
    );
    // Screens: one is the attachment. None means it was never seen, and two
    // means a resume that could not be served.
    for screens in [0, 2] {
        let mut redrawn = passing();
        redrawn.heard.screens = screens;
        assert!(
            judged(&redrawn, "", true).is_err(),
            "exactly one screen is right, and {screens} was taken"
        );
    }
    // Most of the rounds not finishing, and a run that never churned a
    // session — which leaves the daemon weighed for the churn weighing an
    // idle one.
    let mut idle = passing();
    idle.drops = 4;
    assert!(
        judged(&idle, "nothing came back", true).is_err(),
        "a run whose rounds did not finish proved nothing"
    );
    let mut still = passing();
    still.churn = 0;
    assert!(
        judged(&still, "", true).is_err(),
        "a run that made and unmade no session weighs an idle daemon"
    );
    // And rounds that failed one after another, which is what a stack that
    // stopped answering looks like — a failing round is slower than a healthy
    // one, so counting them against the attempts is not enough on its own.
    let mut gone = passing();
    gone.streak = ROUNDS;
    assert!(
        judged(&gone, "nothing came back", true).is_err(),
        "a run of failures one after another is a stack that stopped answering"
    );
}

/// # Panics
///
/// When a side that grew is not caught, or one that was never measured is
/// taken for one that did not grow.
#[test]
fn regression_soak_judges_a_run_by_what_it_weighed() {
    // A side that grew past the ceiling. This is the case that holds the
    // ceiling comparison itself: without it, inverting that comparison leaves
    // every other case in this file green.
    let mut grew = passing();
    grew.server = climbing(SOAK_GROWTH_CEILING_PER_HOUR.saturating_mul(4));
    assert!(
        matches!(judged(&grew, "", true), Err(SoakError::Grew { .. })),
        "a side that grew past the ceiling is refused"
    );
    // And one just under it is not, so the ceiling is a ceiling.
    let mut gentle = passing();
    gentle.server = climbing(SOAK_GROWTH_CEILING_PER_HOUR.saturating_div(2));
    assert!(judged(&gentle, "", true).is_ok(), "and one under it passes");
    // A side never weighed at all, and a side whose samples measure nothing
    // after the warmup — the same silence wearing a series, which would
    // otherwise skip the ceiling entirely.
    // One sample, and late enough that the staleness check is satisfied — so
    // what refuses this report is the branch this case is for and not the one
    // beside it. A line cannot be fitted through a single point.
    let one = vec![Sample {
        at: Duration::from_mins(9),
        bytes: RESIDENT,
    }];
    for spoiling in 0..SIDES {
        let mut unweighed = passing();
        let mut unmeasured = passing();
        match spoiling {
            0 => (unweighed.client, unmeasured.client) = (Vec::new(), one.clone()),
            1 => (unweighed.server, unmeasured.server) = (Vec::new(), one.clone()),
            _ => (unweighed.churned, unmeasured.churned) = (Vec::new(), one.clone()),
        }
        assert!(
            judged(&unweighed, "", true).is_err(),
            "a side never weighed is refused, and side {spoiling} was not"
        );
        assert!(
            judged(&unmeasured, "", true).is_err(),
            "a side measuring nothing after the warmup is refused, and side {spoiling} was not"
        );
        // And a side whose samples stop partway through: a census that
        // stopped finding it leaves a series that ends early, and a rate read
        // from it is a rate from whenever it stopped. This is the check
        // beside the one above, and the two are held apart by when their last
        // sample was taken.
        let mut stopped = passing();
        let early: Vec<Sample> = climbing(0)
            .into_iter()
            .filter(|held| held.at < Duration::from_mins(2))
            .collect();
        match spoiling {
            0 => stopped.client = early,
            1 => stopped.server = early,
            _ => stopped.churned = early,
        }
        assert!(
            judged(&stopped, "", true).is_err(),
            "a side that stopped being found is refused, and side {spoiling} was not"
        );
    }
}

/// # Panics
///
/// When a command line that leaves nothing to measure is taken, or one that
/// cannot overflow is refused.
#[test]
fn regression_soak_refuses_a_warmup_that_swallows_the_run() {
    let asked = |rest: &[&str]| {
        let mut arguments = vec![std::ffi::OsString::from("soak")];
        arguments.extend(rest.iter().map(std::ffi::OsString::from));
        asked_for(&arguments)
    };
    assert!(
        asked(&["--duration", "10", "--warmup", "2"]).is_ok(),
        "a warmup inside the run is taken"
    );
    // The default warmup is ten minutes, so a ten-minute soak asked for
    // without one would measure none of itself and pass for it.
    assert!(
        asked(&["--duration", "10"]).is_err(),
        "a soak no longer than the warmup it would use is refused"
    );
    assert!(
        asked(&["--duration", "10", "--warmup", "10"]).is_err(),
        "and so is one exactly as long as its warmup"
    );
    // A number that would overflow the arithmetic that turns minutes into a
    // duration is clamped rather than panicking on a path whose job is to
    // answer a bad command line with a refusal.
    assert!(
        asked(&["--duration", "99999999999999999999999"]).is_err(),
        "a number no duration can hold is refused rather than panicked on"
    );
    assert!(
        asked(&["--duration", "18446744073709551615"]).is_ok(),
        "and the largest one that can be read is clamped to something sane"
    );
}

/// # Panics
///
/// When one sample that caught a flood reads as a leak.
#[test]
fn regression_soak_reads_a_peak_as_a_peak() {
    // A level series with one sample twice the size at the end of it, which
    // is what a flood in flight looks like. A fit through sixty-one samples
    // moves by a fraction of one of them.
    let mut peaked = climbing(0);
    if let Some(sample) = peaked.last_mut() {
        sample.bytes = sample.bytes.saturating_mul(2);
    }
    let rate = grown(&peaked, NO_WARMUP);
    assert!(
        rate.is_some_and(|held| held < SOAK_GROWTH_CEILING_PER_HOUR),
        "one sample that caught a flood is not a leak: {rate:?}"
    );
    // And its pull falls away as a run lengthens, which is why a six-hour run
    // reads tens of kilobytes an hour for the same one peak.
    let mut longer = climbing(0);
    longer.extend(climbing(0).into_iter().map(|sample| Sample {
        at: sample.at.saturating_add(Duration::from_secs(SERIES)),
        bytes: sample.bytes,
    }));
    if let Some(sample) = longer.last_mut() {
        sample.bytes = sample.bytes.saturating_mul(2);
    }
    let diluted = grown(&longer, NO_WARMUP);
    assert!(
        diluted < rate,
        "a longer series with the same peak reads lower: {diluted:?} against {rate:?}"
    );
    // And growth in the last quarter alone is seen, which is the shape of a
    // leak that begins once a ring has filled. Two middles could not see it.
    let mut late = climbing(0);
    let began = SERIES.saturating_mul(3).saturating_div(4);
    for sample in &mut late {
        if sample.at.as_secs() >= began {
            let over = sample.at.as_secs().saturating_sub(began);
            sample.bytes = sample
                .bytes
                .saturating_add(over.saturating_mul(RESIDENT).saturating_div(SERIES));
        }
    }
    let lately = grown(&late, NO_WARMUP);
    assert!(
        lately.is_some_and(|held| held > SOAK_GROWTH_CEILING_PER_HOUR),
        "growth in the last quarter is growth: {lately:?}"
    );
    // As is growth in the first quarter and nowhere else.
    let mut early = climbing(0);
    let over = SERIES.saturating_div(4);
    for sample in &mut early {
        let moved = sample.at.as_secs().min(over);
        sample.bytes = sample
            .bytes
            .saturating_add(moved.saturating_mul(RESIDENT).saturating_div(SERIES));
    }
    let started = grown(&early, NO_WARMUP);
    assert!(
        started.is_some_and(|held| held > 0),
        "and so is growth in the first: {started:?}"
    );
}

/// # Panics
///
/// When the scenario weighs with anything but the census the soak runs.
#[test]
fn regression_soak_weighs_with_one_census() {
    let said = std::fs::read_to_string(root().join(SCENARIO))
        .unwrap_or_else(|error| panic!("{SCENARIO}: {error}"));
    let read: toml::Value =
        toml::from_str(&said).unwrap_or_else(|error| panic!("{SCENARIO}: {error}"));
    let census = WEIGH.replace(NAME, SERVER_PROCESS);
    // The scenario proves the census works in these containers, and it can
    // only prove it of the text it runs. Transcribing that text is how the
    // two drift apart in the clause that matters — the `--stdio` guard is the
    // whole of what the claim says the weighing has to get right — so the
    // scenario carries this constant word for word and this case says so.
    // Compared after the file is parsed, because what the shell is handed is
    // what the file means and not how it is spelled.
    let weighing = read
        .get("steps")
        .and_then(toml::Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .find(|step| step.get("id").and_then(toml::Value::as_str) == Some(WEIGHING_STEP))
        })
        .and_then(|step| step.get("run"))
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("{SCENARIO} has a {WEIGHING_STEP} step that runs something"));
    assert!(
        weighing.contains(&census),
        "the scenario weighs with the soak's own census.\nit runs: {weighing}\nthe soak's is: {census}"
    );
    // And what is compared is the clause the claim rests on.
    assert!(
        census.contains("--stdio"),
        "the census this compares still leaves the relay out: {census}"
    );
}
