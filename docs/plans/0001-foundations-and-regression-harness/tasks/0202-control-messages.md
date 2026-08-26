---
id: control-messages
title: "Control Messages"
workstream: "0002"
kind: task
depends_on:
  - frame-codec
gated: false
touches:
  - "crates/iznik-protocol/src/identity.rs"
  - "crates/iznik-protocol/src/capabilities.rs"
  - "crates/iznik-protocol/src/message.rs"
  - "crates/iznik-protocol/tests/message_golden.rs"
  - "crates/iznik-protocol/tests/fixtures/message.jsonl"
  - "policy/lexicon/control-messages.txt"
status: planned
merged_as: ""
---
# Control Messages

The `iznik/1` control messages on channel 0, and the rule that pane output on channels 1 to 255 is never parsed. The session-model payloads are carried as opaque byte blobs here so that plan 0003 can define them without changing a byte this fixture pins; the error codes and mark kinds that later plans emit are pinned here for the same reason.

**Steps:**

1. Author `crates/iznik-protocol/tests/fixtures/message.jsonl` first: at least one line per variant of `ToServer` and `ToClient`, one per `ErrorCode` and per `MarkKind`, boundary values for every integer field, an empty and a non-empty opaque payload, a `Hello` with unknown capability bits set, and every error case in `MessageError`.
2. Implement `identity.rs` and `capabilities.rs`, then `message.rs` — the two enums, `ErrorCode`, `MarkKind`, their discriminants, the four `encode`/`decode` functions, `pane_output`, and `MessageError` — exactly as the architecture's `control-messages` section specifies.
3. Write `crates/iznik-protocol/tests/message_golden.rs` asserting the fixture in both directions.

**Tests:**

- Every fixture line round-trips exactly in both directions, with a failure naming the case's `description`.
- Unknown capability bits survive a decode-then-encode round trip unchanged.
- A protocol version other than `1` in `Hello` is decoded and surfaced as a value, not refused — the handshake owns the refusal policy.
- `pane_output` returns a borrow of the payload with no copy, asserted by pointer identity, and refuses channel 0 with `ControlChannel`.
- Each `MessageError` variant is produced by the fixture line that provokes it: an unknown discriminant, an unknown error code, an unknown mark kind, a truncated integer, a truncated string, trailing bytes, invalid UTF-8 in a string field.
- Every encoded message fits inside `MAXIMUM_PAYLOAD_LENGTH` or is refused with `Oversize` before allocation.

- **Done when:** `timeout 600 cargo nextest run --package iznik-protocol --test message_golden` passes every case above and `timeout 3600 cargo xtask check` succeeds.
