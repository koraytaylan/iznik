---
id: multiplexer-assembly
title: "Multiplexer Assembly"
workstream: "0013"
kind: task
depends_on:
  - session-registry
  - channel-multiplexer
  - resume-and-replay
gated: false
touches:
  - "crates/iznik-server/src/multiplexer/mod.rs"
  - "crates/iznik-server/src/multiplexer/scheduler.rs"
  - "crates/iznik-server/tests/multiplexer_assembly.rs"
  - "regression/claims/multiplexer-assembly.toml"
  - "policy/lexicon/multiplexer-assembly.txt"
status: planned
merged_as: ""
---
# Multiplexer Assembly

One connection, one multiplexer, up to 255 subscribed panes: the pump that fans out deltas and marks in order, starts every subscription the way the resume decision says, serves pane bytes by credit with the focused pane first, marks a background pane stale rather than buffering for it, and buffers no pane bytes of its own. Its acceptance is a measured latency figure under adversarial load — a functional test that both panes "work" would pass on an implementation nobody could stand to use.

**Steps:**

1. Implement `crates/iznik-server/src/multiplexer/mod.rs` and `scheduler.rs` — `FrameSink`, `SinkError`, `Multiplexer` with every operation the architecture's `multiplexer-assembly` section lists, the pump with its fan-out and `Lagged` handling, the scheduling round, stale marking and the `Screen` catch-up, and `KEYSTROKE_ROUND_TRIP_BUDGET` — over the channel table, the credit windows and `resume::plan_start`.
2. Write `crates/iznik-server/tests/multiplexer_assembly.rs` with an in-memory `FrameSink`, `sh` panes, generated floods, and the latency case named `keystroke_latency_under_flood`.
3. Declare this task's claims in `regression/claims/multiplexer-assembly.toml` as `test` proofs with their `because`; the same paths over a real link are proven in plans 0004 and 0005.

**Tests:**

- Fan-out order: deltas from the registry arrive on channel 0 in generation order with no gap; a receiver forced past `DELTA_BROADCAST_CAPACITY` is followed by a `Snapshot`; marks arrive only for subscribed panes and carry their sequence.
- Pane exit detaches: a subscribed pane that exits produces `PaneDetached` and the channel awaits acknowledgement.
- Cold attach: `Subscribe` yields `PaneChannel { sequence: newest }`, then a `Screen` whose reproduction equals the mirror, then bytes beginning at exactly that sequence.
- Hot resume: after receiving a stream, unsubscribing, and letting the pane produce more, `Resume { from }` yields the missed bytes with no `Screen`, and the concatenation of before and after equals the history.
- Aged out: `Resume` with a sequence older than the ring holds yields the cold-attach path, and the reproduction through the oracle equals an unbroken stream's.
- `ScreenRequest` on a live subscription yields a `Screen` at `newest` and the cursor moves there, so no byte is delivered twice.
- Exactness under load: during a flood, a `Screen` and the bytes that follow it reassemble, through the oracle, to the same screen as the unbroken stream.
- The latency budget: with one pane flooding and another echoing keystrokes, both subscribed, a thousand keystroke-to-echo round trips report a 99th percentile under `KEYSTROKE_ROUND_TRIP_BUDGET`, with the distribution's tail in the assertion message.
- Fairness: three background panes streaming at equal rates receive throughput within a stated tolerance of each other.
- Focus: moving focus between two flooding panes shifts first service and the larger window to the newly focused one within a stated number of rounds.
- Credit is honored: a cursor whose client stops returning credit stops receiving frames while every other cursor continues, and the multiplexer's memory does not grow.
- Zero credit never spins: a stalled cursor over a 200 millisecond idle interval costs no measurable CPU time.
- Stale catch-up: a background pane flooded past `STALE_THRESHOLD_BYTES` receives no more frames until it is focused, then receives a `PaneChannel` at `newest` and a `Screen` whose reproduction through the oracle equals the pane's mirror, then live bytes.
- Nothing buffered: the multiplexer's own memory does not grow with the flood of an unsubscribed pane, asserted by a memory sample.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test multiplexer_assembly` passes every case above, `timeout 900 cargo xtask claims verify --task multiplexer-assembly` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
