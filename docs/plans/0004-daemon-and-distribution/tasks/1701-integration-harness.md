---
id: integration-harness
title: "Integration Harness"
workstream: "0017"
kind: task
depends_on:
  - stdio-relay
gated: false
touches:
  - "crates/iznik-testkit/src/stack.rs"
  - "crates/iznik-testkit/tests/stack.rs"
  - "crates/iznik-regression/src/step/client.rs"
  - "regression/claims/integration-harness.toml"
  - "regression/scenarios/integration-harness/**"
  - "policy/lexicon/integration-harness.txt"
status: done
merged_as: ""
---
# Integration Harness

A real daemon — in this process or as the binary — and the protocol client, under a deadline: the thing every later integration test is written against, without any test guessing a binary's path. The regression driver gets the same client as a step, which is what lets a scenario on the engine attach to the daemon on the host through real SSH before the client engine exists.

**Steps:**

1. Implement `crates/iznik-testkit/src/stack.rs` — `Stack`, `StackOptions`, `DaemonMode::{InProcess, Binary}`, `StackError`, teardown on drop — exactly as the architecture's `integration-harness` section specifies.
2. Implement `crates/iznik-regression/src/step/client.rs` — `[steps.client]` with `command`, `actions`, `until_quiet_milliseconds`, `capture_to`, `screen_to` and `expect_reassembly` — driving a `TestClient` over the named command's standard streams.
3. Write `crates/iznik-testkit/tests/stack.rs`, and the scenarios under `regression/scenarios/integration-harness/` — `attach-over-ssh` (create a session, subscribe, type a line, capture the bytes and the screen through `ssh host0 /iznik/bin/iznik-server --stdio`) and `reconnect-over-ssh` (drop the network with a fault step, reconnect, resume from the held sequence, assert byte-exact continuation with `expect_reassembly`).
4. Declare this task's claims in `regression/claims/integration-harness.toml`, each proven by one of those scenarios.

**Tests:**

- In-process: the stack starts within `STARTUP_CEILING`, its socket accepts a `TestClient`, and it tears down on drop and on panic, leaving no pane process and no socket.
- Binary mode, from the server package's perspective simulated with the testkit's own path resolution disabled: `DaemonMode::Binary` with a path that does not exist fails with `StackError` naming it rather than hanging.
- Round trip: `command(CreateSession)` answers `Applied` with a session id, `snapshot()` shows it, `subscribe` yields a `PaneChannel` then a `Screen`, `input("echo harness\n")` produces bytes on the channel that, appended to the screen through the oracle, show the echoed line.
- `auto_credit` keeps a flood flowing; without it, the flow stops after the initial window and resumes on an explicit `credit`.
- In the container: `attach-over-ssh` and `reconnect-over-ssh` pass with byte-identical reassembly asserted inside the engine.

- **Done when:** `timeout 600 cargo nextest run --package iznik-testkit --test stack` passes every case above, `timeout 900 cargo xtask claims verify --task integration-harness` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
