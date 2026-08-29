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
    COMPACT, Heard, Report, SOAK_GROWTH_CEILING_PER_HOUR, Sample, SoakError, asked_for, grown,
    heard_more, judged, poured, rendered, soaked,
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
        .map_err(|error| format!("awk: {error}"))?;
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

/// A report of a run that passed, which a case then spoils one way at a time.
fn passing() -> Report {
    let level = climbing(0);
    Report {
        machine: "a machine".to_owned(),
        duration: Duration::from_mins(10),
        warmup: Duration::ZERO,
        rounds: 10,
        drops: 10,
        churn: 10,
        heard: Heard {
            deliveries: 100,
            bytes: poured(30_000).saturating_mul(10),
            screens: 1,
            expected: None,
        },
        client: level.clone(),
        server: level.clone(),
        churned: level,
    }
}

/// # Panics
///
/// When a report that should pass does not, or when spoiling one thing about
/// it is not caught.
#[test]
fn regression_soak_judges_a_run_by_what_it_measured() {
    let passes = passing();
    assert!(
        judged(&passes, "", true).is_ok(),
        "a run that did everything asked of it passes"
    );
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
    // A side that was never weighed at all. A weighing that silently found
    // nothing every time would otherwise report a flat series and no leak.
    for spoiling in 0..3 {
        let mut unweighed = passing();
        match spoiling {
            0 => unweighed.client = Vec::new(),
            1 => unweighed.server = Vec::new(),
            _ => unweighed.churned = Vec::new(),
        }
        assert!(
            judged(&unweighed, "", true).is_err(),
            "a side that was never weighed is refused, and side {spoiling} was not"
        );
    }
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
    // A second screen: a reconnection the host could not carry on from.
    let mut redrawn = passing();
    redrawn.heard.screens = 2;
    assert!(
        judged(&redrawn, "", true).is_err(),
        "a screen after the attachment is a resume that could not be served"
    );
    // And most of the rounds not finishing.
    let mut idle = passing();
    idle.drops = 4;
    assert!(
        judged(&idle, "nothing came back", true).is_err(),
        "a run whose rounds did not finish proved nothing"
    );
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
    // A level series with one sample twice the size in its later half, which
    // is what a flood in flight looks like. Taking the upper of the two
    // middle samples would make that peak the answer.
    let mut peaked = climbing(0);
    if let Some(sample) = peaked.last_mut() {
        sample.bytes = sample.bytes.saturating_mul(2);
    }
    let rate = grown(&peaked, NO_WARMUP);
    assert_eq!(
        rate,
        Some(0),
        "a level series with one peak in it grew by nothing: {rate:?}"
    );
    // And a series of four, where each half is two samples, so the middle of
    // each is between them rather than the larger of them.
    let four = vec![
        Sample {
            at: Duration::from_secs(0),
            bytes: RESIDENT,
        },
        Sample {
            at: Duration::from_mins(1),
            bytes: RESIDENT,
        },
        Sample {
            at: Duration::from_mins(2),
            bytes: RESIDENT,
        },
        Sample {
            at: Duration::from_mins(3),
            bytes: RESIDENT.saturating_mul(2),
        },
    ];
    let halved = grown(&four, NO_WARMUP);
    assert!(
        halved.is_some_and(|held| held < SOAK_GROWTH_CEILING_PER_HOUR.saturating_mul(200)),
        "and a half of two is not read as its larger sample: {halved:?}"
    );
}
