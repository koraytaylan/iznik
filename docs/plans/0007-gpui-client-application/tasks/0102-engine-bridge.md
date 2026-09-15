---
id: engine-bridge
title: "Bridge the engine: HostManager on tokio, events into GPUI entities"
workstream: "0001"
kind: task
depends_on:
  - gpui-adoption
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/bridge.rs
  - crates/iznik-app/src/host_ui.rs
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/tests/bridge.rs
  - policy/lexicon/engine-bridge.txt
  - regression/claims/engine-bridge.toml
status: planned
merged_as: ""
---
# Bridge the engine: HostManager on tokio, events into GPUI entities

Give the application its engine: a private tokio runtime owning a `HostManager`, `ManagerEvent`s forwarded to GPUI entities over a channel, and the host lifecycle surfaced as application state — all headless-testable against the in-process stack over a `unix:` alias.

**Steps:**

1. Write `crates/iznik-app/src/bridge.rs`: the runtime task constructing `ManagerOptions` and `HostManager`, draining `events()`, and forwarding every `ManagerEvent` to a channel the main thread drains inside its update cycle; document the one rule — the main thread never calls the engine on a code path that waits on the engine's own task.
2. Write `crates/iznik-app/src/host_ui.rs`: the application's host entities — per-host connection state, the client model mirror (`ClientModel`) updated from snapshots and deltas, and notifications derived from `ManagerEvent`s, including the upgrade offer a host returns with its connection.
3. Wire the public surface the UI will call: add host, remove host, reconnect, upgrade, uninstall, and the session command submission path through `HostManager::command`.
4. Write `crates/iznik-app/tests/bridge.rs`: against the `iznik-testkit` in-process stack, add the `unix:` host, observe the state transitions, create a session by command, see it in the model mirror, and see a refusal surface as a notification — all inside GPUI's test context.
5. Declare this task's claims in `regression/claims/engine-bridge.toml`.

**Tests:**

- Snapshot and delta application converge the model mirror exactly as `iznik-client`'s own reducer tests do.
- A refused command leaves the model unchanged and produces a notification naming the refusal.
- The upgrade offer arrives as state a dialog can render, distinct from an error.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task engine-bridge` reports every claim proven.
