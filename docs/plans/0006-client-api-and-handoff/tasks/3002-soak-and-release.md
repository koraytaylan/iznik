---
id: soak-and-release
title: "Soak and Release"
workstream: "0030"
kind: task
depends_on:
  - static-library-and-header
  - diagnostics-bundle
gated: false
touches:
  - "xtask/src/soak.rs"
  - "xtask/tests/regression_soak.rs"
  - "docs/notes/release-checklist.md"
  - "docs/notes/soak.md"
  - "regression/claims/soak-and-release.toml"
  - "regression/scenarios/soak-and-release/**"
  - "policy/lexicon/soak-and-release.txt"
status: done
merged_as: ""
---
# Soak and Release

A system like this fails on the timescale of days, not tests: a leak of a few kilobytes per reconnect is invisible in CI and fatal by Thursday. The soak runs the whole stack for hours with drops, churn and a flood, and the release checklist begins with its report. The run this task commits is ten minutes — what fits beside a task's gates inside its wall clock — and the six-hour run is a person's.

**Steps:**

1. Implement `xtask::soak` — the schedule, the churn, the flood, memory sampling on both sides with each sample printed as it is taken, the growth ceiling and the byte-loss check, `--duration` and `--warmup` — and fill the `xtask soak` subcommand, exactly as the architecture's `soak-and-release` section specifies, over the fixture and `manager` steps.
2. Run `xtask soak --duration 10 --warmup 2` on this machine and commit its report to `docs/notes/soak.md`; write `docs/notes/release-checklist.md`, whose first item is a six-hour soak run by a person before a release.
3. Write `xtask/tests/regression_soak.rs`, `#[ignore]`, running a one-minute soak, and the scenario under `regression/scenarios/soak-and-release/` — `short-soak`, with `exclusive = true` — as the registered proof; declare this task's claims in `regression/claims/soak-and-release.toml`.

**Tests:**

- The one-minute soak completes with drops, churn and the flood all exercised, memory sampled at least once, and no byte lost across any reconnection.
- The growth check fails a synthetic sample series that grows past the ceiling and passes one that plateaus.
- `docs/notes/soak.md` carries a report of at least ten minutes with the machine described, the duration, the warmup, the drop count, the pane churn count, and both memory series.
- The release checklist names the soak report, the baseline, `cargo xtask check`, `xtask claims coverage`, the distribution artifacts and the golden header, in that order.

- **Done when:** `timeout 600 cargo nextest run --package xtask --test regression_soak --run-ignored all` passes, `timeout 900 cargo xtask claims verify --task soak-and-release` reports every claim proven, `docs/notes/soak.md` reports a soak of at least ten minutes, and `timeout 3600 cargo xtask check` succeeds.
