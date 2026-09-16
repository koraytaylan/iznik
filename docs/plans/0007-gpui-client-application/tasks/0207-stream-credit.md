---
id: stream-credit
title: "Bind consumed output credit to its delivering stream"
workstream: "0002"
kind: task
depends_on:
  - engine-bridge
  - vt-thread
gated: true
touches:
  - crates/iznik-client/README.md
  - crates/iznik-client/src/host/manager/mod.rs
  - crates/iznik-client/src/host/manager/task.rs
  - crates/iznik-client/src/host/manager/credit.rs
  - crates/iznik-client/tests/fixtures/stream_credit.rs
  - crates/iznik-client/tests/fixtures/stream_host.rs
  - crates/iznik-client/tests/stream_credit.rs
  - crates/iznik-client/tests/connection_manager.rs
  - crates/iznik-client/tests/manager_traffic.rs
  - crates/iznik-app/README.md
  - crates/iznik-app/src/bridge.rs
  - crates/iznik-app/src/host_ui.rs
  - crates/iznik-app/src/window.rs
  - crates/iznik-app/src/vt.rs
  - crates/iznik-app/src/grid/mod.rs
  - crates/iznik-app/src/surface.rs
  - crates/iznik-app/src/input.rs
  - crates/iznik-app/tests/support/mod.rs
  - crates/iznik-app/tests/stream_credit.rs
  - crates/iznik-app/tests/surface.rs
  - crates/iznik-app/tests/ime.rs
  - crates/iznik-app/tests/input_encoding.rs
  - crates/iznik-app/tests/grid_element.rs
  - crates/iznik-app/tests/window_shell.rs
  - crates/iznik-app/tests/vt_thread.rs
  - crates/iznik-app/benches/grid_budget.rs
  - crates/iznik-cli/src/tail.rs
  - crates/iznik-cli/src/benchmark.rs
  - crates/iznik-regression/src/step/manager.rs
  - crates/iznik-ffi/src/shape.rs
  - crates/iznik-ffi/src/lib.rs
  - ARCHITECTURE.md
  - policy/lexicon/stream-credit.txt
  - regression/claims/stream-credit.toml
status: done
merged_as: ""
---
# Bind consumed output credit to its delivering stream

The GPUI and VT queues can retain an old frame after the engine has reopened
or reassigned its pane channel. The existing `credit(pane, bytes)` resolves
the current channel at call time; its queued order retains only that channel
number. Returning an old frame can therefore replenish a new stream, and a
queued grant can cross another channel reassignment before it reaches the
wire. A UI-side disconnect flag cannot fix this race: the engine may already
have processed the replacement while the UI still holds earlier events.

**Steps:**

1. Commit the transition fixture in `tests/fixtures/stream_credit.rs` before
   implementing receipt bookkeeping. Its expected grants are authoritative.
2. Mint an opaque delivery receipt tied to the exact stream incarnation and
   output byte count. Stream identity must not repeat after channel reuse,
   reconnect, host removal/re-addition or manager replacement. Cloned receipts
   share return state, so one delivery can earn credit only once.
3. Validate receipt ownership when the host task is about to carry its credit
   order, not only when the application submits it. Expire streams on link
   loss, detachment and channel replacement. Record successful grants at that
   owning boundary; a failed order submission must not consume a receipt.
4. Carry receipts with engine output through the app bridge and native VT
   snapshot. The grid retains them until the corresponding snapshot is
   accepted. Retry preserves pending receipts, duplicate frames add none,
   and screen resets never create credit. Expired receipts can be retired
   without granting credit to the new stream.
5. Preserve the wire protocol and C ABI. The existing pane-count credit API
   keeps its current-stream meaning but its queued orders also retain stream
   identity. The Rust application uses the stronger delivery-receipt path;
   compatibility consumers may ignore the new event metadata.
6. Prove bookkeeping with the fixed fixture, order validation with a scripted
   transport, and app receipt propagation with headless tests. The window's
   real reconnect/backpressure acceptance still uses the container fixture.

**Tests:**

- Every fixture history yields exactly its named grants: current once,
  channel replacement, channel reassignment, identical-number reuse,
  disconnect, detach, host isolation, manager replacement and control channel.
- A receipt queued before stream replacement is rejected at carrying time.
- Failed submission leaves a receipt retryable; duplicate queued returns do
  not grant twice and accounting does not advance on a rejected submission.
- Engine delivery carries the exact byte count and stream identity through
  the bridge, native snapshot and accepted grid consumption.
- Old accepted frames cannot refill a replacement channel; an unrelated pane
  continues earning its own credit.
- The C ABI and wire goldens remain unchanged.

**Done when:** `timeout 600 cargo nextest run --package iznik-client
--package iznik-app`, `timeout 900 cargo xtask claims verify --task
stream-credit`, and `timeout 3600 cargo xtask check` all pass, with every
acceptance case above implemented and the transport scenario proven.

## Acceptance

The engine and app acceptance suites pass all 158 tests. Six task claims
cover the fixed histories, queued admission, actual framed transport grants,
bridge/native/grid propagation, retry, reset and receipt validation. The wire
proof also holds an independent pane across replacement and checks model
accounting before rejected submission and after successful writes. Protocol
and C-header goldens pass unchanged in the workspace gate.

The queued-admission proof puts receipts in a channel before replacing the
stream, then drains them through the production admission ledger. The framed
peer proof independently checks that the manager uses that ledger at the wire
boundary. Removing the identity guard makes that wire proof send old bytes to
the replacement channel, and fail; the restored guard passes.

The application's reconnect, backpressure and competing-resize scenarios
remain acceptance of the window/input tasks, not a claim of this correction.
