---
id: session-command-codec
title: "Session Command Codec"
workstream: "0011"
kind: task
depends_on:
  - model-types
gated: false
touches:
  - "crates/iznik-protocol/src/command.rs"
  - "crates/iznik-protocol/tests/command_golden.rs"
  - "crates/iznik-protocol/tests/fixtures/command.jsonl"
  - "regression/claims/session-command-codec.toml"
  - "policy/lexicon/session-command-codec.txt"
status: planned
merged_as: ""
---
# Session Command Codec

The vocabulary a client uses to change the model and the answer it gets back, encoded as the `Command` and `CommandResult` payloads plan 0001 left opaque. Every command names stable identity; every outcome says what was created or why nothing was.

**Steps:**

1. Author `crates/iznik-protocol/tests/fixtures/command.jsonl` first: one line per `SessionCommand` variant, one per `Created` and `RejectionCode` form, and `Placement` in both directions and both orders.
2. Implement `crates/iznik-protocol/src/command.rs` — `SessionCommand`, `Placement`, `CommandOutcome`, `Created`, `RejectionCode`, and the codec — exactly as the architecture's `session-command-codec` section specifies.
3. Write `crates/iznik-protocol/tests/command_golden.rs`.
4. Declare this task's claims in `regression/claims/session-command-codec.toml` as `test` proofs with their `because`.

**Tests:**

- Every fixture line round-trips exactly in both directions.
- The encodings fit the opaque payloads `message.jsonl` pins: wrapping an encoded command in `ToServer::Command` and decoding it back yields the command, and the `message.jsonl` goldens are unchanged.
- Every error the decoder can produce — an unknown variant, a truncated field, trailing bytes, invalid UTF-8 in a name — is provoked by a fixture line.

- **Done when:** `timeout 600 cargo nextest run --package iznik-protocol --test command_golden` passes every case above, `timeout 900 cargo xtask claims verify --task session-command-codec` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
