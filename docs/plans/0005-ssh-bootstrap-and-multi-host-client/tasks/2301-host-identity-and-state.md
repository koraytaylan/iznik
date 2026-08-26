---
id: host-identity-and-state
title: "Host Identity and State"
workstream: "0023"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-client/src/host/mod.rs"
  - "crates/iznik-client/src/host/identity.rs"
  - "crates/iznik-client/src/host/state.rs"
  - "crates/iznik-client/tests/host_state.rs"
  - "regression/claims/host-identity-and-state.toml"
  - "policy/lexicon/host-identity-and-state.txt"
status: planned
merged_as: ""
---
# Host Identity and State

A pane is addressed globally as `iznik://<host>/<pane>`, and a host's life — probing, bootstrapping, connected, reconnecting with backoff, failed with a retry time — is a pure state machine tested as a table, so that what happens when a laptop closes is decided here and not discovered in the manager.

**Steps:**

1. Implement `crates/iznik-client/src/host/mod.rs`, `identity.rs` and `state.rs` — `HostId` with `local_socket`, `GlobalPaneId` with its rendering and parser, `HostState`, `UpgradeOffer`, `HostEvent`, `Action`, `BackoffPolicy` with its constants as defaults, `HostStateMachine` — exactly as the architecture's `host-identity-and-state` section specifies.
2. Write `crates/iznik-client/tests/host_state.rs` as a transition table over synthetic instants.
3. Declare this task's claims in `regression/claims/host-identity-and-state.toml` as `test` proofs with their `because`.

**Tests:**

- `GlobalPaneId` renders as `iznik://<host>/<pane>` and parses back exactly; an alias with characters that need escaping round-trips; malformed strings are refused; `unix:` aliases render and parse like any other.
- The table: every `(state, event)` pair the architecture defines produces the expected next state and actions, including bootstrap failure at each stage, a dead channel from `Connected`, and a successful reconnection resetting the attempt count.
- Backoff: successive failures produce retry delays growing from `initial` toward `maximum` with jitter within a stated band, never beyond the maximum, and a policy with a 10 millisecond initial value schedules accordingly — the test never sleeps.
- Removal from any state yields exactly the teardown actions that state requires.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test host_state` passes every case above, `timeout 900 cargo xtask claims verify --task host-identity-and-state` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
