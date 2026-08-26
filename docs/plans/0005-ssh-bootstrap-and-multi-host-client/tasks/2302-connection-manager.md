---
id: connection-manager
title: "Connection Manager"
workstream: "0023"
kind: task
depends_on:
  - client-reducer
  - optimistic-commands
  - host-identity-and-state
  - remote-launch
gated: false
touches:
  - "crates/iznik-client/src/host/manager.rs"
  - "crates/iznik-client/tests/connection_manager.rs"
  - "regression/claims/connection-manager.toml"
  - "policy/lexicon/connection-manager.txt"
status: planned
merged_as: ""
---
# Connection Manager

Several hosts at once, each on its own task running its own state machine, with one host's failure, slowness or bootstrap invisible to the others — and on reconnection, a `Resume` for every subscription at the cursor the model holds, which is the mechanism that keeps a pane's bytes across a drop.

**Steps:**

1. Implement `crates/iznik-client/src/host/manager.rs` — `ManagerOptions` with every timing a field, `HostManager` owning its runtime, `ManagerEvent`, `ManagerError`, the per-host task, and every operation the architecture's `connection-manager` section lists — driving the state machine, the bootstrap, the channel, the reducer and the optimistic command functions.
2. Write `crates/iznik-client/tests/connection_manager.rs` against two in-process `Stack`s through `unix:` aliases, with backoff in tens of milliseconds and pong deadlines in hundreds.
3. Declare this task's claims in `regression/claims/connection-manager.toml` as `test` proofs whose `because` says isolation and resume are properties of the manager that a local transport exhibits; the same over SSH is `end-to-end-ssh`.

**Tests:**

- Two hosts: sessions created on each appear only in their own view; a command to one is answered by that one.
- Isolation: with one stack's daemon task paused, the other host's keystroke echo stays under the latency budget and no manager operation on it waits.
- Resume: after a host's socket is closed under it, the manager reconnects with backoff and every subscribed pane's bytes continue from the held cursor, byte-exact through the oracle, under the same `GlobalPaneId`, within two seconds.
- Optimistic in the loop: a rename shows in `model()` before the server answers and stays after; a rename the server rejects is rolled back; a command against a paused host expires with `CommandTimedOut` after the configured timeout.
- No retry storm: four hosts failing at once schedule retries with jitter, asserted by the spread of their retry times.
- Events: every state transition is reported on `events()` in order, and `model()` reflects the latest reduction.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test connection_manager` passes every case above, `timeout 900 cargo xtask claims verify --task connection-manager` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
