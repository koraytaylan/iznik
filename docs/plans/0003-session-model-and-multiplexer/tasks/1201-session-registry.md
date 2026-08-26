---
id: session-registry
title: "Session Registry"
workstream: "0012"
kind: task
depends_on:
  - deltas-and-reconciler
  - session-command-codec
gated: false
touches:
  - "crates/iznik-server/src/session/mod.rs"
  - "crates/iznik-server/src/session/registry.rs"
  - "crates/iznik-server/tests/session_registry.rs"
  - "regression/claims/session-registry.toml"
  - "policy/lexicon/session-registry.txt"
status: planned
merged_as: ""
---
# Session Registry

The server's authoritative model and the operations that change it. Every operation mints identity, spawns or closes real panes, keeps the layout normalized, emits its deltas in a defined order, and applies each delta to the registry's own model through the protocol's reconciler before anyone else sees it — which is what makes the convergence property something the registry can be held to.

**Steps:**

1. Implement `crates/iznik-server/src/session/mod.rs` and `registry.rs` — `Registry`, `RegistryDefaults`, `Numbered`, `DELTA_BROADCAST_CAPACITY`, every operation, the removal cascade, and pane-event ingestion — exactly as the architecture's `session-registry` section specifies.
2. Write `crates/iznik-server/tests/session_registry.rs`, with `RegistryDefaults { program: sh }` everywhere but the ingestion case and `iznik_testkit::generate` for the operation sequences.
3. Declare this task's claims in `regression/claims/session-registry.toml` as `test` proofs with their `because`.

**Tests:**

- Convergence: for two hundred generated operation sequences of up to twenty operations, applying every emitted delta with the protocol's reconciler to a copy of the previous snapshot yields exactly the registry's next snapshot, every snapshot validates, and the case finishes in under five seconds.
- Delta order: each operation emits exactly the deltas the architecture lists, in that order, with consecutive generations.
- Cascade: a pane's exit removes it with `Exited`; the last pane's exit removes its tab; the last tab's removal removes its session; the deltas arrive in that order.
- Placement: a pane created before and after a target, in both directions, produces the expected normalized layout; creating into a same-direction split flattens.
- `set_layout` with a leaf set other than the tab's panes is refused and nothing changes; a valid unnormalized layout is stored normalized.
- Ingestion: a title mark and a directory mark from a `bash --rcfile` integration shell become `PaneTitle` and `PaneWorkingDirectory` deltas; a resize becomes `PaneResized`.
- Identity: ids are never reused across create, close and create again.
- Lag: a delta receiver that falls behind by more than `DELTA_BROADCAST_CAPACITY` observes `Lagged` and nothing in the registry is disturbed.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test session_registry` passes every case above, `timeout 900 cargo xtask claims verify --task session-registry` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
