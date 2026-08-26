---
id: gate-runner
title: "Gate Runner"
workstream: "0001"
kind: task
depends_on:
  - workspace-scaffold
gated: false
touches:
  - "crates/iznik-harness/src/process.rs"
  - "crates/iznik-harness/src/deadline.rs"
  - "crates/iznik-harness/tests/process.rs"
  - "crates/iznik-harness/tests/deadline.rs"
  - "xtask/src/gate.rs"
  - "xtask/src/doctor.rs"
  - "xtask/tests/gate_runner.rs"
  - "xtask/tests/doctor.rs"
  - "policy/lexicon/gate-runner.txt"
status: done
merged_as: ""
---
# Gate Runner

Nothing in this repository waits without a bound. This task lands the one place a child process is spawned synchronously — with a deadline that kills the whole process group and reports what the child said before it died — and the two deadline helpers every fixture wait is written with, and builds `cargo xtask check`, `cargo xtask gate` and `cargo xtask doctor` on them, so the command a person runs before committing is the command Makina runs before landing.

**Steps:**

1. Implement `iznik_harness::process` — `Deadline`, `Output::{Inherit, Capture}`, `run`, `Completed`, `ProcessError`, `TERMINATION_GRACE` — exactly as the architecture's `gate-runner` section specifies: the child in its own process group, `SIGTERM` to the group at the deadline, `SIGKILL` after the grace interval, output streamed through as it happens under `Inherit`, and captured with a size cap under `Capture`.
2. Implement `iznik_harness::deadline` — `run_capped`, `wait_until`, `DeadlineError::Elapsed { cap, context }`.
3. Implement `xtask::gate` — `Gate::{Format, Lint, Documentation, Test, Claims}` with `command()` and `deadline()` matching `CONTRIBUTING.md` §2, `run_gate`, and `check` — and `xtask::doctor` with one `Prerequisite` per row of `CONTRIBUTING.md` §1, each with a probe command and an install hint, the `podman` probe also checking for the `netavark` network backend.
4. Fill the `xtask check`, `xtask gate <name>` and `xtask doctor` subcommands; `check` runs the doctor first and stops at the first missing prerequisite or failing gate, naming it.

**Tests:**

- A child that exits promptly returns `Completed` with its status, output and elapsed time; a child that exceeds its deadline is reported as `TimedOut` within the deadline plus the grace interval, and neither it nor its grandchildren survive — asserted by a census of the process group.
- Under `Inherit`, a child's output reaches the parent's streams before the child exits, asserted with a child that prints, sleeps past a threshold, then prints again.
- Under `Capture`, output beyond the cap is truncated with the tail kept and the truncation stated.
- A program that cannot be spawned is `Spawn` with its name; a child exiting non-zero is `Failed` with its status and stderr tail.
- `run_capped` returns the body's value when it finishes in time and `Elapsed` with its context when it does not, within the cap plus a stated slack; `wait_until` returns as soon as the condition holds and `Elapsed` naming the context at the cap. Every deadline in these tests is under a second.
- The gate table in `xtask::gate` and the `[[gates]]` in `.makina/config.toml` agree on names, order, commands and deadlines, asserted by parsing the configuration file — a gate that Makina runs but `check` does not is how "it passed locally" stops meaning anything.
- `check` stops at the first failing gate and its output names the gate; with every gate passing it prints one line per gate with elapsed time and exits 0.
- The doctor reports every missing prerequisite by name with its install command and exits non-zero, asserted with a `PATH` that hides one tool; with everything present it exits 0.

- **Done when:** `timeout 600 cargo nextest run --package iznik-harness -E 'test(process) | test(deadline)'` and `timeout 600 cargo nextest run --package xtask -E 'test(gate_runner) | test(doctor)'` pass every case above, and `timeout 3600 cargo xtask check` succeeds end to end on this tree.
