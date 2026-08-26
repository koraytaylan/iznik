---
id: performance-baseline
title: "Performance Baseline"
workstream: "0017"
kind: task
depends_on:
  - integration-harness
gated: false
touches:
  - "crates/iznik-server/benches/baseline.rs"
  - "crates/iznik-server/tests/regression_baseline.rs"
  - "docs/notes/baseline.md"
  - "regression/claims/performance-baseline.toml"
  - "policy/lexicon/performance-baseline.txt"
status: planned
merged_as: ""
---
# Performance Baseline

Performance claims in this repository are measured, committed numbers — never adjectives. This task measures them through the real daemon binary over the real socket under the profile the containers run, commits them with the machine described, and asserts ceilings generous enough that the regression test survives a noisy machine and strict enough that a real regression fails it.

**Steps:**

1. Implement `crates/iznik-server/benches/baseline.rs` against a `Stack` in `Binary` mode, reporting every figure the architecture's `performance-baseline` section lists as a Markdown table.
2. Run it with `cargo bench --profile regression` on this machine and commit the table to `docs/notes/baseline.md` with the machine described: processor, memory, kernel, toolchain, the commit measured.
3. Write `crates/iznik-server/tests/regression_baseline.rs`, `#[ignore]`, every test in it named with `baseline` so nextest runs it alone, asserting the named ceilings, and declare this task's claims in `regression/claims/performance-baseline.toml` as `test` proofs with `profile = "regression"` and a `because` saying the ceilings are properties of the process, not of a network.

**Tests:**

- Idle latency: the 99th percentile keystroke-to-echo round trip with no other load is under `IDLE_LATENCY_CEILING`.
- Flood latency: with another pane flooding at line rate, the 99th percentile is under `FLOOD_LATENCY_CEILING`, with the distribution in the assertion message.
- Throughput: a single pane sustains at least `SINGLE_PANE_THROUGHPUT_FLOOR` through the socket; eight panes together sustain more than one.
- Memory: the daemon's resident memory at rest is under `RESTING_MEMORY_CEILING`; with fifty idle panes it is under `FIFTY_PANE_MEMORY_CEILING`.
- Startup: `--foreground` to socket connectable is under `STARTUP_CEILING`.
- `docs/notes/baseline.md` contains every figure the bench reports, asserted by parsing the table.

- **Done when:** `timeout 900 cargo bench --profile regression --package iznik-server --bench baseline` prints the table, `timeout 900 cargo nextest run --cargo-profile regression --package iznik-server --test regression_baseline --run-ignored all` passes every ceiling, `timeout 900 cargo xtask claims verify --task performance-baseline` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
