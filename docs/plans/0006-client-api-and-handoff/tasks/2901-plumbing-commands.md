---
id: plumbing-commands
title: "Plumbing Commands"
workstream: "0029"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-cli/src/output.rs"
  - "crates/iznik-cli/src/probe.rs"
  - "crates/iznik-cli/src/state.rs"
  - "crates/iznik-cli/src/tail.rs"
  - "crates/iznik-cli/src/benchmark.rs"
  - "crates/iznik-cli/src/uninstall.rs"
  - "crates/iznik-cli/tests/plumbing.rs"
  - "regression/claims/plumbing-commands.toml"
  - "regression/scenarios/plumbing-commands/**"
  - "policy/lexicon/plumbing-commands.txt"
status: done
merged_as: ""
---
# Plumbing Commands

Structured output for a person or a script, never a screen: iznik has exactly one user interface and it is the macOS application. These commands exist so the stack can be driven and inspected from a shell when the application is not the thing under test.

**Steps:**

1. Implement `crates/iznik-cli/src/output.rs` — the one place the binary writes, one JSON object per line built by hand through `std::io::Write` — and `probe.rs`, `state.rs`, `tail.rs`, `benchmark.rs`, `uninstall.rs` over `iznik-client`, exactly as the architecture's `plumbing-commands` section specifies, filling the dispatcher's stubs.
2. Write `crates/iznik-cli/tests/plumbing.rs` against an in-process `Stack` through a `unix:` alias with `env!("CARGO_BIN_EXE_iznik")`, and the scenarios under `regression/scenarios/plumbing-commands/` — `probe-over-ssh`, `tail-over-ssh` — running `/iznik/bin/iznik` in the engine.
3. Declare this task's claims in `regression/claims/plumbing-commands.toml`.

**Tests:**

- Every command's stdout is valid JSON, one object per line, and stderr is empty on success; on failure stderr carries one JSON object with the layer and the message and the exit code is non-zero.
- `probe` prints the `HostProbe`; `state` prints the client model; `tail` prints a pane's bytes as they arrive and exits 0 on `SIGINT`; `benchmark` prints the round-trip distribution with median and 99th percentile; `uninstall` leaves nothing on the host.
- No command reads a terminal or waits for input; each runs to completion with stdin closed.
- In the container: `probe` and `tail` over real SSH from the engine print what the local cases print.

- **Done when:** `timeout 600 cargo nextest run --package iznik-cli --test plumbing` passes every local case, `timeout 900 cargo xtask claims verify --task plumbing-commands` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
