---
id: remote-channel
title: "Remote Channel"
workstream: "0020"
kind: task
depends_on:
  - ssh-control-master
gated: false
touches:
  - "crates/iznik-client/src/transport/channel.rs"
  - "crates/iznik-client/tests/remote_channel.rs"
  - "crates/iznik-regression/src/step/channel.rs"
  - "regression/claims/remote-channel.toml"
  - "regression/scenarios/remote-channel/**"
  - "policy/lexicon/remote-channel.txt"
status: done
merged_as: ""
---
# Remote Channel

One channel per host carrying `iznik/1` through `iznik-server --stdio` — or straight to a local socket — with the handshake, negotiated compression through the shared link layer, and liveness that notices a dead link in seconds.

**Steps:**

1. Implement `crates/iznik-client/src/transport/channel.rs` — `ChannelOptions` with every timing a field, `RemoteChannel` with `open`, `send`, `next`, `close`, the handshake, compression through `iznik_link::compression::compressed`, the `Ping`/`Pong` liveness, `ChannelError` — exactly as the architecture's `remote-channel` section specifies.
2. Implement `crates/iznik-regression/src/step/channel.rs` — `[steps.channel]` with `alias`, `server`, `actions`, the two timing fields and `expect_dead_within_milliseconds`.
3. Write `crates/iznik-client/tests/remote_channel.rs` against an in-process `Stack` through `Transport::Local`, with `ping_interval` and `pong_deadline` in the hundreds of milliseconds, and the scenarios under `regression/scenarios/remote-channel/` — `handshake-over-ssh`, `bytes-over-ssh`, `dead-link-noticed` — from the engine with the same short timings.
4. Declare this task's claims in `regression/claims/remote-channel.toml`.

**Tests:**

- Over a local transport: the handshake completes, a subscribed pane's bytes arrive byte-identical to the history, frames split at every boundary reassemble, and compression is engaged when the daemon advertises `ZSTD` and not otherwise.
- A mismatched protocol version — a scripted server over a duplex — is `ProtocolVersion` naming the host and the server's version.
- Liveness: with the daemon's task paused so nothing answers, `next` returns `Dead` within `pong_deadline` plus one `ping_interval`, naming the host and the silence, in under a second of wall clock.
- In the container: the handshake and a pane's bytes through real SSH; after a network fault, `Dead` is surfaced within the configured deadline rather than after a TCP timeout, and a new channel opens after the fault is cleared.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test remote_channel` passes every local case, `timeout 900 cargo xtask claims verify --task remote-channel` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
