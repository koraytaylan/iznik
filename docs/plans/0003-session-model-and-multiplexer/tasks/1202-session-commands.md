---
id: session-commands
title: "Session Commands"
workstream: "0012"
kind: task
depends_on:
  - session-registry
gated: false
touches:
  - "crates/iznik-server/src/session/commands.rs"
  - "crates/iznik-server/tests/session_commands.rs"
  - "regression/claims/session-commands.toml"
  - "policy/lexicon/session-commands.txt"
status: planned
merged_as: ""
---
# Session Commands

A command from a client is validated against the model, applied, and answered exactly once with what it created or why it was refused. A rejected command changes nothing — not the generation, not a pane, not a delta — because a half-applied command is the one thing a client cannot reconcile.

**Steps:**

1. Implement `crates/iznik-server/src/session/commands.rs` — `apply` with validation, the mapping of every `SessionCommand` to its registry operation, and every `RejectionCode` — exactly as the architecture's `session-commands` section specifies.
2. Write `crates/iznik-server/tests/session_commands.rs`, its registry defaulting to `sh`.
3. Declare this task's claims in `regression/claims/session-commands.toml` as `test` proofs with their `because`.

**Tests:**

- Happy path per command: each of the eleven commands is applied, answers `Applied` with the right `created` and generation, and the snapshot reflects it.
- Rejection per code: an unknown session, tab and pane; an empty name; a non-permutation order; a layout with the wrong leaves; a working directory that does not exist — each answers `Rejected` with the expected code, and the generation and delta stream are unchanged.
- Spawn failure is reported: with `RegistryDefaults { program: Command { path: "/nonexistent" } }`, `CreatePane` answers `SpawnFailed` with the pseudoterminal error's message and nothing is added.
- Exactly once: a thousand rename commands produce exactly a thousand outcomes, in order, in under a second.
- Spawn parameters: a pane created with a size and a directory has a shell whose `stty size` and `pwd` report them.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test session_commands` passes every case above, `timeout 900 cargo xtask claims verify --task session-commands` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
