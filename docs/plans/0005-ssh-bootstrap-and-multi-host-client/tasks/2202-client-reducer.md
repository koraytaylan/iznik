---
id: client-reducer
title: "Client Reducer"
workstream: "0022"
kind: task
depends_on:
  - client-model
gated: false
touches:
  - "crates/iznik-client/src/reduce.rs"
  - "crates/iznik-client/tests/client_reducer.rs"
  - "regression/claims/client-reducer.toml"
  - "policy/lexicon/client-reducer.txt"
status: planned
merged_as: ""
---
# Client Reducer

The server's messages applied to the client's model with the protocol's own reconciler, so the convergence property proven in plan 0003 carries over unchanged, and the effects — request a snapshot, release a channel, reset to a screen, notify — returned as values for the manager to act on.

**Steps:**

1. Implement `crates/iznik-client/src/reduce.rs` — `reduce`, `Effect`, `Notification` — exactly as the architecture's `client-reducer` section specifies.
2. Write `crates/iznik-client/tests/client_reducer.rs` over `iznik_testkit::generate`.
3. Declare this task's claims in `regression/claims/client-reducer.toml` as `test` proofs with their `because`.

**Tests:**

- Convergence: for a thousand generated delta sequences delivered as `Delta` messages, the host view equals the model the generator built; a `Snapshot` in the middle replaces and continues; under five seconds all told.
- Gap: a delta with a skipped generation yields `RequestSnapshot` and leaves the view unchanged; the snapshot that follows resolves it.
- Subscriptions: `PaneChannel` opens one at its sequence; pane bytes advance the cursor by their length; `PaneDetached` drops it and yields `ReleaseChannel`; `Screen` resets the cursor to its sequence and yields `Screen`.
- Routing: messages for one host never touch another host's view.
- `CommandResult` and `Mark` yield notifications carrying the host and the identity.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test client_reducer` passes every case above, `timeout 900 cargo xtask claims verify --task client-reducer` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
