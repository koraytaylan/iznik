//! The VT oracle against its goldens: every case snapshots as authored, the
//! accessors agree with the snapshot, two oracles fed the same bytes agree
//! byte for byte and say nothing about addresses or time, the format is
//! versioned on its first line, and a flood snapshots in under a second.

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use iznik_testkit::golden;
use iznik_testkit::vt::{SNAPSHOT_VERSION, Vt};
use serde_json::Value;

/// The golden fixture, relative to this crate.
const FIXTURE: &str = "tests/fixtures/vt/cases.jsonl";

/// The inventory's flood: four mebibytes of generated text.
const FLOOD_BYTES: usize = 4 * 1024 * 1024;

/// The inventory's flood terminal: 200 columns.
const FLOOD_COLUMNS: u16 = 200;

/// The inventory's flood terminal: 50 rows.
const FLOOD_ROWS: u16 = 50;

/// The inventory's ceiling on feeding the flood and snapshotting it.
const FLOOD_CEILING: Duration = Duration::from_secs(1);

/// How many digits in a row make a number look like a time or an address.
const SUSPICIOUS_DIGITS: usize = 8;

/// Why a case could not be run.
type Failure = Box<dyn Error>;

/// One golden case.
struct Case {
    /// What it says it is.
    description: String,
    /// The columns.
    columns: u16,
    /// The rows.
    rows: u16,
    /// The steps, in order.
    steps: Vec<Step>,
    /// The expected snapshot.
    snapshot: String,
}

/// One step of a case.
enum Step {
    /// Feed bytes.
    Feed(Vec<u8>),
    /// Resize to columns and rows.
    Resize(u16, u16),
}

/// A narrow integer field.
///
/// # Errors
///
/// When the field is absent, not an integer, or too wide.
fn narrow_field<Number: TryFrom<u64>>(object: &Value, name: &str) -> Result<Number, Failure> {
    let wide = object
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("field `{name}` is not an unsigned integer"))?;
    Number::try_from(wide).map_err(|_error| format!("field `{name}` does not fit its width").into())
}

/// A string field.
///
/// # Errors
///
/// When the field is absent or not a string.
fn string_field(object: &Value, name: &str) -> Result<String, Failure> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("field `{name}` is not a string").into())
}

/// The step a JSON object describes.
///
/// # Errors
///
/// When the object is neither a feed nor a resize.
fn step(value: &Value) -> Result<Step, Failure> {
    if let Some(hex) = value.get("feed").and_then(Value::as_str) {
        return Ok(Step::Feed(golden::bytes(hex)?));
    }
    if let Some(size) = value.get("resize").and_then(Value::as_array) {
        let columns = size
            .first()
            .and_then(Value::as_u64)
            .ok_or("resize lacks columns")?;
        let rows = size
            .get(1)
            .and_then(Value::as_u64)
            .ok_or("resize lacks rows")?;
        return Ok(Step::Resize(u16::try_from(columns)?, u16::try_from(rows)?));
    }
    Err("a step is a feed or a resize".into())
}

/// The fixture's cases, in order.
///
/// # Errors
///
/// When the fixture cannot be loaded or a line is not a case.
fn cases() -> Result<Vec<Case>, Failure> {
    golden::lines(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE))?
        .iter()
        .map(|value| {
            let steps = value
                .get("steps")
                .and_then(Value::as_array)
                .ok_or("a case lists its steps")?
                .iter()
                .map(step)
                .collect::<Result<Vec<Step>, Failure>>()?;
            Ok(Case {
                description: string_field(value, "description")?,
                columns: narrow_field(value, "columns")?,
                rows: narrow_field(value, "rows")?,
                steps,
                snapshot: string_field(value, "snapshot")?,
            })
        })
        .collect()
}

/// An oracle after a case's steps.
///
/// # Errors
///
/// The oracle's error.
fn run(case: &Case) -> Result<Vt, Failure> {
    let mut vt = Vt::new(case.columns, case.rows)?;
    for step in &case.steps {
        match step {
            Step::Feed(bytes) => vt.feed(bytes),
            Step::Resize(columns, rows) => vt.resize(*columns, *rows)?,
        }
    }
    Ok(vt)
}

/// A snapshot rebuilt from the accessors alone, in the snapshot's format.
///
/// # Errors
///
/// The oracle's error.
fn rebuilt(vt: &Vt) -> Result<String, Failure> {
    let (columns, rows) = vt.size()?;
    let (cursor_column, cursor_row) = vt.cursor()?;
    let mut text = format!(
        "{SNAPSHOT_VERSION}\nsize {columns}x{rows}\ncursor {cursor_column},{cursor_row}\ntitle {:?}\nworking_directory {:?}\nscrollback {}\n",
        vt.title()?,
        vt.working_directory()?,
        vt.scrollback_rows()?
    );
    let mut legend = String::new();
    for row in 0..rows {
        let _row_written = writeln!(text, "{row}|{}|", vt.row_text(row)?);
        for column in 0..columns {
            let attributes = vt.cell(column, row)?.attributes();
            if !attributes.is_empty() {
                let _legend_written = writeln!(legend, "{column},{row} {}", attributes.join(" "));
            }
        }
    }
    text.push_str("attributes\n");
    text.push_str(&legend);
    Ok(text)
}

/// Every golden snapshots exactly as authored.
///
/// # Panics
///
/// When a case's snapshot differs from the golden; the message names the
/// case and shows both.
#[test]
fn vt_oracle_goldens_snapshot_as_authored() {
    let cases = cases().expect("the fixture loads");
    assert!(cases.len() >= 10, "the fixture has {} cases", cases.len());
    let mut mismatches = String::new();
    for case in &cases {
        let vt = run(case).unwrap_or_else(|error| panic!("{}: {error}", case.description));
        let snapshot = vt.snapshot().expect("snapshots");
        if snapshot != case.snapshot {
            let _written = writeln!(
                mismatches,
                "{}:\n--- expected\n{}--- actual\n{snapshot}",
                case.description, case.snapshot
            );
        }
    }
    assert!(mismatches.is_empty(), "{mismatches}");
}

/// `cell`, `row_text`, `screen_text`, `cursor`, `title`,
/// `working_directory` and `scrollback_rows` agree with the snapshot for
/// every golden.
///
/// # Panics
///
/// When the snapshot rebuilt from the accessors differs from `snapshot`, or
/// `screen_text` is not the rows joined.
#[test]
fn vt_oracle_accessors_agree_with_the_snapshot() {
    for case in &cases().expect("the fixture loads") {
        let vt = run(case).unwrap_or_else(|error| panic!("{}: {error}", case.description));
        let snapshot = vt.snapshot().expect("snapshots");
        let from_accessors = rebuilt(&vt).expect("the accessors answer");
        assert!(
            from_accessors == snapshot,
            "{}:\n--- from the accessors\n{from_accessors}--- snapshot\n{snapshot}",
            case.description
        );
        let (_columns, rows) = vt.size().expect("sizes");
        let rows_joined: Vec<String> = (0..rows)
            .map(|row| vt.row_text(row).expect("a row"))
            .collect();
        assert_eq!(
            vt.screen_text().expect("the screen"),
            rows_joined.join("\n"),
            "{}",
            case.description
        );
    }
}

/// Two oracles fed the same bytes produce byte-identical snapshots, and a
/// snapshot contains no pointer, address, capacity or time.
///
/// # Panics
///
/// When the snapshots differ, or one carries a forbidden pattern.
#[test]
fn vt_oracle_snapshots_are_deterministic_and_carry_no_addresses() {
    let bytes = b"one \x1b[1mtwo\x1b[0m\r\n\x1b[38;5;9mthree\x1b[0m \xe6\xbc\xa2\r\n\x1b]0;t\x07";
    let mut first = Vt::new(20, 4).expect("an oracle");
    let mut second = Vt::new(20, 4).expect("an oracle");
    first.feed(bytes);
    second.feed(bytes);
    let left = first.snapshot().expect("snapshots");
    let right = second.snapshot().expect("snapshots");
    assert_eq!(left, right);
    for forbidden in ["ptr", "capacity", "Instant", "SystemTime"] {
        assert!(
            !left.contains(forbidden),
            "the snapshot carries `{forbidden}`:\n{left}"
        );
    }
    let longest_digit_run = left
        .split(|character: char| !character.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0);
    assert!(
        longest_digit_run < SUSPICIOUS_DIGITS,
        "the snapshot carries a run of {longest_digit_run} digits:\n{left}"
    );
    for (offset, _text) in left.match_indices("0x") {
        let before = left
            .get(..offset)
            .and_then(|prefix| prefix.chars().next_back());
        assert!(
            before.is_some_and(|character| character.is_ascii_digit()),
            "the snapshot carries an address at byte {offset}:\n{left}"
        );
    }
}

/// A snapshot is versioned `vt/1` on its first line.
///
/// # Panics
///
/// When a golden's first line is not the version.
#[test]
fn vt_oracle_snapshots_are_versioned() {
    assert_eq!(SNAPSHOT_VERSION, "vt/1");
    for case in &cases().expect("the fixture loads") {
        assert_eq!(
            case.snapshot.lines().next(),
            Some(SNAPSHOT_VERSION),
            "{}",
            case.description
        );
        let vt = run(case).unwrap_or_else(|error| panic!("{}: {error}", case.description));
        assert_eq!(
            vt.snapshot().expect("snapshots").lines().next(),
            Some(SNAPSHOT_VERSION)
        );
    }
}

/// An oracle of 200 columns by 50 rows fed 4 MiB of generated text
/// snapshots in under a second; a baseline, so nextest gives it the machine.
///
/// # Panics
///
/// When the feed and the snapshot together take the ceiling or more.
#[test]
fn vt_oracle_flood_baseline_snapshots_in_under_a_second() {
    let line = b"the quick brown fox jumps over the lazy dog 0123456789\r\n";
    let mut flood = Vec::with_capacity(FLOOD_BYTES);
    while flood.len() < FLOOD_BYTES {
        flood.extend_from_slice(line);
    }
    let started = Instant::now();
    let mut vt = Vt::new(FLOOD_COLUMNS, FLOOD_ROWS).expect("an oracle");
    vt.feed(&flood);
    let snapshot = vt.snapshot().expect("snapshots");
    let elapsed = started.elapsed();
    assert!(snapshot.starts_with(SNAPSHOT_VERSION));
    assert!(elapsed < FLOOD_CEILING, "the flood took {elapsed:?}");
}
