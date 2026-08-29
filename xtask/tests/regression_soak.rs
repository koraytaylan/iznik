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
    COMPACT, SOAK_GROWTH_CEILING_PER_HOUR, Sample, grown, heard_in, rendered, soaked,
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
        "client",
        "server",
    ] {
        assert!(said.contains(named), "the report carries {named}: {said}");
    }
}

/// # Panics
///
/// When a stream that jumped is called whole, or a whole one is called
/// broken.
#[test]
fn regression_soak_notices_a_stream_that_jumped() {
    // Two deliveries of four bytes each, the second beginning where the first
    // ended: whole.
    let whole = "output 0 8 2\noutput 4 8 2\n";
    let heard = heard_in(whole).unwrap_or_else(|gap| panic!("{gap}"));
    assert_eq!(
        (heard.deliveries, heard.bytes),
        (2, 8),
        "and what it heard is counted: {heard:?}"
    );
    // The same stream with a byte missing from the middle is not.
    assert!(
        heard_in("output 0 8 2\noutput 5 8 2\n").is_err(),
        "a delivery that does not begin where the one before it ended is a loss"
    );
    // A screen is the host sending the truth rather than catching a client
    // up, so it moves the cursor legitimately.
    assert!(
        heard_in("output 0 8 2\nscreen 99 4 2\noutput 100 8 2\n").is_ok(),
        "and a screen begins the reckoning again rather than breaking it"
    );
    // Nothing at all is nothing heard, not a stream that was whole.
    let nothing = heard_in("").unwrap_or_else(|gap| panic!("{gap}"));
    assert_eq!(nothing.deliveries, 0, "nothing heard is nothing heard");
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
                   \"bytes\":\"ZWZnaGlqa2w=\",\"encoding\":\"base64\"}\n\
                   {\"kind\":\"detached\",\"pane\":1}\n";
    let reduced = compacted(printed).unwrap_or_else(|error| panic!("{error}"));
    let heard = heard_in(&reduced).unwrap_or_else(|gap| panic!("{gap}"));
    // Four bytes then eight, the second beginning where the first ended, and
    // the screen before them starting the reckoning rather than counting.
    assert_eq!(
        (heard.deliveries, heard.bytes),
        (2, 12),
        "what the filter wrote is what the check reads: {heard:?}"
    );
    // And a delivery that does not follow the one before it is still caught
    // after passing through the filter.
    let jumped = printed.replace("\"sequence\":5", "\"sequence\":6");
    let broken = compacted(&jumped).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        heard_in(&broken).is_err(),
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
    let directory = std::env::temp_dir().join("iznik-soak-compact");
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
        "## The client, in bytes",
        "## The server, in bytes",
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
