---
id: channel-cancellation
title: "Preserve channel frames when the manager cancels a receive"
workstream: "0002"
kind: task
depends_on:
  - engine-bridge
gated: true
touches:
  - crates/iznik-client/src/transport/channel.rs
  - crates/iznik-client/tests/channel_cancellation.rs
  - crates/iznik-client/tests/fixtures/channel_cancellation.rs
  - crates/iznik-client/README.md
  - crates/iznik-ffi/tests/pane_byte_pipe.rs
  - crates/iznik-ffi/tests/fixtures/pane_notices.rs
  - crates/iznik-link/src/compression.rs
  - crates/iznik-link/tests/compression.rs
  - crates/iznik-link/tests/fixtures/decode_pause.rs
  - crates/iznik-link/README.md
  - policy/lexicon/channel-cancellation.txt
  - regression/claims/channel-cancellation.toml
status: done
merged_as: ""
---
# Preserve channel frames when the manager cancels a receive

The application gate reproduced lost output in the existing FFI concurrent
input case, including once in ten isolated stress runs. The manager races
`RemoteChannel::next` against outgoing orders. The channel consumes a frame,
then awaits a mutex to record its receive time. That await can yield under
Tokio's cooperative budget even without lock contention; cancellation there
can discard already-consumed output. The receive-time value has only one
owner, despite currently being wrapped in an asynchronous shared mutex.

**Steps:**

1. Commit the three-frame fixture before implementing the correction.
2. Build a deterministic local-peer proof: prime buffered frames, leave one
   cooperative operation available, poll then cancel a pending receive, and
   verify that the complete frame sequence survives. Establish failure before
   changing the channel; do not rely solely on the intermittent FFI test.
3. Remove unnecessary asynchronous ownership of the receive timestamp so no
   suspension point follows consuming a deliverable frame. Keep liveness,
   deadlines and the wire protocol unchanged.
4. Document the cancellation obligation and add a test claim. The local
   scheduling proof needs no container; existing container/channel proofs
   still cover the actual SSH transport.

**Tests:**

- The fixed frame sequence is complete and ordered after cancellation under
  an exhausted cooperative budget.
- Restoring the post-consumption await makes the focused proof fail.
- Existing channel deadline, ping, compression and peer-failure cases pass.
- Repeated idle compressed polls stay pending before and after output, preserve
  subsequent bytes and accept clean shutdown; the old decoder fails this proof.
- The existing FFI atomic-input case passes ten isolated stress iterations.

**Done when:** `timeout 300 cargo nextest run --package iznik-client --test
channel_cancellation --test remote_channel`, `timeout 240 cargo nextest run
--package iznik-ffi --test pane_byte_pipe -E
"test(pane_byte_pipe_keeps_one_call_one_message)" --stress-count 10`,
`timeout 300 cargo nextest run --package iznik-link --test compression`,
`timeout 900 cargo xtask claims verify --task channel-cancellation`, and
`timeout 3600 cargo xtask check` all pass.

The first cancellation repair passes the deterministic counterexample, but a
subsequent ten-run FFI stress check still lost a tail once. Keep this task
open and use the FFI test's isolated server history to locate that remaining
failure before claiming the original intermittent symptom is resolved.

## Verified boundary and remaining investigation

The cooperative-budget fixture fails in milliseconds on the old channel,
losing the middle frame and returning the third next. Direct timestamp
ownership passes all eight channel tests. Restoring the old await reproduces
the exact failure; the repaired source is restored afterward.

This fixes a demonstrated cancellation defect but does not yet close the
whole task. The existing FFI input stress test still failed once in ten after
the repair. A temporary independent client resumed the failed pane from zero:
its retained server history was missing the same caller patterns, locating
the remaining symptom before FFI output delivery. Further temporary tracing
changed scheduling and did not reproduce it in thirty runs. All diagnostic
instrumentation has been removed. Do not treat those passing traced runs as
evidence that the input-loss symptom has been repaired.

A later stress failure contained every caller's pattern plus a spurious
combined line: kernel echo and `cat` output legitimately interleaved on the
same terminal. The atomic-input fixture therefore disables terminal echo,
waits for an unambiguous post-change marker, then verifies its probe through
the reader. It now requires exactly one returned line per caller, keeping
all missing, extra and interleaved-input checks. This corrects a fixture
assumption about two independent output producers, not the input contract.

## Compressed idle-poll diagnosis

Low-overhead engine notifications captured reconnects during the failing
input burst. Failure-only diagnostics then recorded the actual channel error:
zstd rejected repeated empty decoder calls for making no progress. The
manager may cancel and repoll a pending receive whenever an outgoing order
arrives. The compressed reader must remember that it needs new bytes after
exhausting buffered input, while still draining plaintext the decoder already
holds. Commit `tests/fixtures/decode_pause.rs`, prove repeated idle polls fail
before repair, then verify that idle cancellation remains pending and output
sent afterward still decodes exactly. Add this proof to the task's claims.
The failure-only diagnostic prints are removed before the correction lands.

## Completion evidence

Both cancellation boundaries are repaired. The idle decoder fixture fails in
four milliseconds on the old source with the observed no-progress error;
restoring that source reproduces the failure again. The repaired decoder
waits for transport input after exhaustion while retaining buffered-output
draining and the clean-versus-truncated close distinction. All eight
compression cases pass, including corpus, large payload and handshake leftovers.
The FFI atomic-input case passes thirty isolated runs without production
diagnostics. All five workspace gates pass: 538 tests and 70 branch claims,
with the two existing display measurements still explicitly deferred.
