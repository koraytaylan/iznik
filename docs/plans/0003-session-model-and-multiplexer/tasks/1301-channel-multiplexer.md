---
id: channel-multiplexer
title: "Channel Multiplexer"
workstream: "0013"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-server/src/multiplexer/channel.rs"
  - "crates/iznik-server/src/multiplexer/credit.rs"
  - "crates/iznik-server/tests/channel_multiplexer.rs"
  - "regression/claims/channel-multiplexer.toml"
  - "policy/lexicon/channel-multiplexer.txt"
status: done
merged_as: ""
---
# Channel Multiplexer

The pure tables the multiplexer is built on: channel numbers assigned on subscribe and released only after the client acknowledges the detach — so a late frame can never be misattributed to a new pane — and the per-cursor credit windows with every constant the scheduler will honor. No pane, no sink, no runtime: these tests run in milliseconds and the pump that uses the tables lands with `multiplexer-assembly`.

**Steps:**

1. Implement `crates/iznik-server/src/multiplexer/channel.rs` and `credit.rs` — `ChannelTable`, `Cursor`, `CreditWindow`, `MultiplexerError`, and the constants `INITIAL_CREDIT_BYTES`, `FOCUSED_CREDIT_BYTES`, `FRAME_PAYLOAD_LENGTH`, `STALE_THRESHOLD_BYTES` — exactly as the architecture's `channel-multiplexer` section specifies.
2. Write `crates/iznik-server/tests/channel_multiplexer.rs`.
3. Declare this task's claims in `regression/claims/channel-multiplexer.toml` as `test` proofs with their `because`.

**Tests:**

- Assignment: subscriptions receive distinct channels from 1 upward; the 256th is `ChannelsExhausted`; `channel_of` and `pane_of` agree in both directions.
- Release discipline: after `release`, the channel is not reassigned until `acknowledge`; an assignment in between receives a different channel; acknowledging a channel that was not released is an error naming it.
- Credit arithmetic: `consume` never exceeds `available`, `refill` saturates rather than overflows, and a focused window is `FOCUSED_CREDIT_BYTES` while a background one is `INITIAL_CREDIT_BYTES`.
- Constants: `FRAME_PAYLOAD_LENGTH` is below `MAXIMUM_PAYLOAD_LENGTH` and `STALE_THRESHOLD_BYTES` is at least `FOCUSED_CREDIT_BYTES`, asserted so a later edit cannot make a stale mark unreachable.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test channel_multiplexer` passes every case above, `timeout 900 cargo xtask claims verify --task channel-multiplexer` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
