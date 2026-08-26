//! The scenario format: what parses, what does not and why, and what each
//! assertion means against a hand-written record — none of it needing a
//! container.

use std::path::Path;

use iznik_harness::report::Record;
use iznik_harness::scenario::{
    Expect, MAXIMUM_SCENARIO_BUDGET_SECONDS, Scenario, ScenarioError, evaluate, parse,
};

/// A well-formed scenario, as a base for the malformed variants.
const WELL_FORMED: &str = r#"
name = "example"
claims = ["example-claim"]
driver = "engine"
budget_seconds = 30

[setup]
hosts = 1
files = []

[[steps]]
id = "one"
container = "engine"
run = "true"
timeout_seconds = 10

[[expect]]
step = "one"
exit = 0
"#;

/// The message a parse of `text` fails with.
///
/// # Errors
///
/// When the text parses instead of failing.
fn refusal(text: &str) -> Result<String, String> {
    match parse(text) {
        Ok(_scenario) => Err("the malformed scenario parsed".to_owned()),
        Err(error) => Ok(error.to_string()),
    }
}

/// A record for the step named, with the given streams and outcome.
fn record(
    step: &str,
    exit: Option<i32>,
    timed_out: bool,
    milliseconds: u64,
    stdout: &str,
    stderr: &str,
) -> Record {
    Record {
        scenario: "example".to_owned(),
        step: step.to_owned(),
        exit,
        timed_out,
        duration_milliseconds: milliseconds,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    }
}

/// A well-formed scenario parses, and `exclusive` defaults to false.
///
/// # Panics
///
/// When it does not parse, or a default is wrong.
#[test]
fn scenario_format_a_well_formed_scenario_parses() {
    let scenario = parse(WELL_FORMED).expect("the scenario parses");
    assert_eq!(scenario.name, "example");
    assert_eq!(scenario.driver, "engine");
    assert!(!scenario.exclusive, "exclusive defaults to false");
    assert_eq!(scenario.steps.len(), 1);
    assert_eq!(scenario.steps[0].kind, "run");
    assert_eq!(scenario.claims, ["example-claim"]);
}

/// An unknown key, at the top or in a table, is refused by name.
///
/// # Panics
///
/// When any is accepted, or the message does not name the key.
#[test]
fn scenario_format_an_unknown_key_is_refused_by_name() {
    let top = WELL_FORMED.replace("driver = \"engine\"", "driver = \"engine\"\nmystery = 1");
    assert!(refusal(&top).expect("refused").contains("mystery"));
    let setup = WELL_FORMED.replace("hosts = 1", "hosts = 1\nmystery = 2");
    assert!(refusal(&setup).expect("refused").contains("mystery"));
    let step = WELL_FORMED.replace("timeout_seconds = 10", "timeout_seconds = 10\nmystery = 3");
    assert!(refusal(&step).expect("refused").contains("mystery"));
    let expectation = WELL_FORMED.replace("exit = 0", "exit = 0\nmystery = 4");
    assert!(refusal(&expectation).expect("refused").contains("mystery"));
}

/// A step without `timeout_seconds` and a scenario without `budget_seconds`
/// are refused, naming what is missing.
///
/// # Panics
///
/// When either is accepted.
#[test]
fn scenario_format_a_missing_deadline_or_budget_is_refused() {
    let no_timeout = WELL_FORMED.replace("timeout_seconds = 10\n", "");
    assert!(
        refusal(&no_timeout)
            .expect("refused")
            .contains("timeout_seconds")
    );
    let no_budget = WELL_FORMED.replace("budget_seconds = 30\n", "");
    assert!(
        refusal(&no_budget)
            .expect("refused")
            .contains("budget_seconds")
    );
}

/// A budget over the maximum does not parse.
///
/// # Panics
///
/// When it is accepted, or the message does not name the maximum.
#[test]
fn scenario_format_a_budget_over_the_maximum_is_refused() {
    let over = WELL_FORMED.replace(
        "budget_seconds = 30",
        &format!("budget_seconds = {}", MAXIMUM_SCENARIO_BUDGET_SECONDS + 1),
    );
    let message = refusal(&over).expect("refused");
    assert!(
        message.contains(&MAXIMUM_SCENARIO_BUDGET_SECONDS.to_string()),
        "{message}"
    );
}

/// A step with two kinds, or none, is refused as such.
///
/// # Panics
///
/// When either is accepted.
#[test]
fn scenario_format_a_step_has_exactly_one_kind() {
    let two = WELL_FORMED.replace("run = \"true\"", "run = \"true\"\nprobe = \"x\"");
    let message = refusal(&two).expect("refused");
    assert!(message.contains("two kinds"), "{message}");
    let none = WELL_FORMED.replace("run = \"true\"\n", "");
    assert!(refusal(&none).expect("refused").contains("no kind"));
    let unknown = WELL_FORMED.replace("run = \"true\"", "mystery = \"true\"");
    assert!(refusal(&unknown).expect("refused").contains("mystery"));
}

/// An assertion naming a step that does not exist is refused.
///
/// # Panics
///
/// When it is accepted, or the message does not name the step.
#[test]
fn scenario_format_an_expect_names_a_known_step() {
    let ghost = WELL_FORMED.replace("step = \"one\"", "step = \"ghost\"");
    assert!(refusal(&ghost).expect("refused").contains("ghost"));
}

/// Two steps sharing an id are refused.
///
/// # Panics
///
/// When accepted.
#[test]
fn scenario_format_two_steps_may_not_share_an_id() {
    let doubled = WELL_FORMED.replace(
        "[[expect]]\nstep = \"one\"\nexit = 0\n",
        "[[steps]]\nid = \"one\"\ncontainer = \"engine\"\nrun = \"false\"\ntimeout_seconds = 5\n",
    );
    assert!(refusal(&doubled).expect("refused").contains("one"));
}

/// A file that does not parse is refused, and one that is not TOML is too.
///
/// # Panics
///
/// When the loader accepts a missing file, or the parser accepts non-TOML.
#[test]
fn scenario_format_loading_and_non_toml_fail() {
    let error = iznik_harness::scenario::load(Path::new("/nonexistent/scenario.toml"))
        .expect_err("a missing file fails to load");
    assert!(matches!(error, ScenarioError::Read { .. }), "{error}");
    let not_toml = parse("this is not toml =").expect_err("non-TOML fails");
    assert!(matches!(not_toml, ScenarioError::Toml { .. }), "{not_toml}");
}

/// The one expectation a scenario with `body` in place of `exit = 0` holds.
///
/// # Errors
///
/// When the scenario does not parse or holds no expectation.
fn expectation(body: &str) -> Result<Expect, String> {
    parse(&WELL_FORMED.replace("exit = 0", body))
        .map_err(|error| error.to_string())?
        .expect
        .into_iter()
        .next()
        .ok_or_else(|| "the scenario holds no expectation".to_owned())
}

/// Every assertion passes on a record that satisfies it.
///
/// # Panics
///
/// When any assertion fails on a record it should accept.
#[test]
fn scenario_format_assertions_pass_on_matching_records() {
    let matching = record("one", Some(0), false, 500, "hello world", "a warning");
    for body in [
        "exit = 0",
        "stdout_equals = \"hello world\"",
        "stdout_contains = \"world\"",
        "stdout_matches = \"^hello .*d$\"",
        "stderr_equals = \"a warning\"",
        "stderr_contains = \"warn\"",
        "stderr_matches = \"warning$\"",
        "duration_under_seconds = 1",
    ] {
        let expect = expectation(body).unwrap_or_else(|error| panic!("{body}: {error}"));
        evaluate(&expect, &matching).unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}

/// Every assertion fails on a record that does not satisfy it, the failure
/// naming what it is about.
///
/// # Panics
///
/// When any assertion passes on a record it should reject, or the failure
/// does not name the assertion.
#[test]
fn scenario_format_assertions_fail_on_mismatching_records() {
    let matching = record("one", Some(0), false, 500, "hello world", "a warning");
    for (body, needle) in [
        ("exit = 1", "exit"),
        ("stdout_equals = \"nope\"", "stdout"),
        ("stdout_contains = \"absent\"", "contain"),
        ("stdout_matches = \"^world\"", "match"),
        ("stderr_equals = \"other\"", "stderr"),
        ("duration_under_seconds = 0", "took"),
        ("stdout_matches = \"(\"", "regex"),
    ] {
        let expect = expectation(body).expect(body);
        let failure = evaluate(&expect, &matching).expect_err(body);
        assert!(failure.contains(needle), "{body}: {failure}");
    }
}

/// A record round-trips through NDJSON.
///
/// # Panics
///
/// When the record does not survive a serialize and parse.
#[test]
fn scenario_format_a_record_round_trips_through_ndjson() {
    let original = record("one", Some(3), true, 1200, "out", "err");
    let line = original.to_ndjson().expect("serializes");
    assert!(!line.contains('\n'), "a record is one line");
    let parsed = Record::from_ndjson(&line).expect("parses");
    assert_eq!(parsed, original);
    let _unused: &Scenario = &parse(WELL_FORMED).expect("parses");
}
