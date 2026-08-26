---
id: scenario-driver
title: "Scenario Driver"
workstream: "0004"
kind: task
depends_on:
  - regression-fixture
gated: false
touches:
  - "crates/iznik-harness/src/scenario.rs"
  - "crates/iznik-harness/src/report.rs"
  - "crates/iznik-harness/src/runner.rs"
  - "crates/iznik-harness/tests/scenario_format.rs"
  - "crates/iznik-regression/src/**"
  - "crates/iznik-regression/tests/regression_scenarios.rs"
  - "regression/scenarios/scenario-driver/**"
  - "policy/lexicon/scenario-driver.txt"
status: planned
merged_as: ""
---
# Scenario Driver

A scenario is data, not code, so an acceptance criterion is something a reviewer can read without trusting the harness that runs it — and every scenario is an ordinary nextest test, so there is one runner, one deadline mechanism, one report and free parallelism. Every step declares its deadline and every scenario its budget; a step that exceeds its deadline is killed and recorded with what it managed to say, and a scenario's overhead beyond its steps is measured and bounded.

**Steps:**

1. Author the scenarios first under `regression/scenarios/scenario-driver/`, each with `budget_seconds` of 60 or less: `round-trip` (a `run` step on the engine), `both-containers` (a step on each container), `cross-container-ssh` (an engine step whose command is `ssh host0 hostname`), `ordering` (three steps whose records must arrive in order), `expected-non-zero` (a step expected to exit 1), `timed-out` (`sleep 5` with `timeout_seconds = 1`), `fault` (a disconnect, a failing `ssh` with a two-second `ConnectTimeout`, a reconnect), `pid-file-kill` (a step that writes `$$` to a file and sleeps, killed through `IdFile`), and `budget` (`budget_seconds = 2` with a step that sleeps ten).
2. Implement `iznik_harness::scenario` (every table denying unknown keys, the mandatory `timeout_seconds` and `budget_seconds`, `MAXIMUM_SCENARIO_BUDGET_SECONDS`, `exclusive`, the exactly-one-kind rule), `iznik_harness::report` (the record), and `iznik_harness::runner` (stage, fixture, file copy, steps through `podman exec … iznik-regression step` or the fixture's faults, `expect`, the budget, the overhead figure) exactly as the architecture's `scenario-driver` section specifies.
3. Implement the `iznik-regression` driver: `step/mod.rs` with the dispatcher, `Context` and `StepError`, `step/run.rs`, the eight `Unsupported` stubs, and the `iznik-regression step` subcommand reading one step as TOML on stdin and printing one NDJSON record.
4. Write `crates/iznik-harness/tests/scenario_format.rs` for the format, and `crates/iznik-regression/tests/regression_scenarios.rs` — the `harness = false` binary over `libtest-mimic` that registers one ignored test named `scenario::<task-id>::<name>` per scenario file, runs it through the runner, and asserts the overhead under `SCENARIO_OVERHEAD_CEILING`.

**Tests:**

- Format, none of it needing podman: a scenario with an unknown key, a step without `timeout_seconds`, a scenario without `budget_seconds` or with one over the maximum, a step with two kinds or none, or an `expect` naming an unknown step fails to parse with a message naming the offense; `exclusive` defaults to false.
- Discovery: `cargo nextest list` shows one test per scenario file with the expected name, and a scenario added to the tree appears without any code change.
- Each scenario above produces exactly the records its steps predict, in order, with `exit`, `timed_out`, `duration_milliseconds`, `stdout` and `stderr` as expected; the timed-out step's record carries `timed_out = true` and its partial output, its process is gone from the container's census, and its scenario completes in under ten seconds.
- A cross-container step runs over real SSH, asserted by the record's `stdout` naming the other container.
- `expect` evaluation: `exit`, `stdout_equals`, `stdout_contains`, `stdout_matches`, the `stderr_` forms and `duration_under_seconds` each pass and fail on the cases that should make them, tested against hand-written records without a fixture.
- The budget: `budget` fails naming the step that was running, in under five seconds, and the fixture is torn down.
- Overhead: every scenario's overhead figure is under `SCENARIO_OVERHEAD_CEILING` and is printed, with the eight scenarios of this task running four at a time.
- Unsupported kinds: a step of a kind whose module is still a stub yields a record naming the kind and the plan that fills it, never a hang.

- **Done when:** `timeout 600 cargo nextest run --package iznik-harness --test scenario_format` passes every format case, `timeout 900 cargo nextest run --package iznik-regression --test regression_scenarios --run-ignored all -E 'test(/^scenario::scenario-driver::/)'` passes every scenario above, and `timeout 3600 cargo xtask check` succeeds.
