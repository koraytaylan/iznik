---
id: optimistic-commands
title: "Optimistic Commands"
workstream: "0022"
kind: task
depends_on:
  - client-model
gated: false
touches:
  - "crates/iznik-client/src/commands.rs"
  - "crates/iznik-client/tests/optimistic_commands.rs"
  - "regression/claims/optimistic-commands.toml"
  - "policy/lexicon/optimistic-commands.txt"
status: done
merged_as: ""
---
# Optimistic Commands

What makes a remote session feel local: a rename, a close or a reorder shows at once, is recorded as pending, and is confirmed or rolled back by the authoritative answer. Creation waits one round trip because a placeholder id is more flicker than waiting, and a command that is never answered is rolled back and surfaced rather than left pending forever. Pure functions over a host view and a clock, landed before the manager that calls them.

**Steps:**

1. Implement `crates/iznik-client/src/commands.rs` — `submit`, `Submission`, `PendingCommand` with its rollback state, `confirm`, `expire`, `PENDING_COMMAND_TIMEOUT` — exactly as the architecture's `optimistic-commands` section specifies.
2. Write `crates/iznik-client/tests/optimistic_commands.rs` against host views built by `iznik_testkit::generate` and synthetic instants.
3. Declare this task's claims in `regression/claims/optimistic-commands.toml` as `test` proofs with their `because`.

**Tests:**

- Each optimistic command's local effect equals the delta the server would send, so confirming with the matching `Applied` and applying that delta leaves the view unchanged.
- A rejection rolls the local effect back and the authoritative state stands.
- Creation, `MovePane` and `SetLayout` are not applied locally and the view changes only on the server's delta.
- Timeout: a pending command with no answer is rolled back by `expire` at a synthetic `now` past the timeout and yields `CommandTimedOut`; one inside the timeout is untouched.
- Two overlapping optimistic commands on the same tab are confirmed in order, and a rejection of the first rolls back only its effect.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test optimistic_commands` passes every case above, `timeout 900 cargo xtask claims verify --task optimistic-commands` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
