---
id: pty-harness
title: "PTY Harness"
workstream: "0003"
kind: task
depends_on:
  - gate-runner
gated: false
touches:
  - "crates/iznik-testkit/src/pty.rs"
  - "crates/iznik-testkit/src/metrics.rs"
  - "crates/iznik-testkit/tests/pty_harness.rs"
  - "crates/iznik-testkit/tests/metrics.rs"
  - "policy/lexicon/pty-harness.txt"
status: done
merged_as: ""
---
# PTY Harness

Every integration test from plan 0002 onward drives a real process on a real pseudoterminal, and it has to be deterministic: tests assert on content, never on wall-clock timing. `read_until_quiet` is the primitive the whole suite leans on, and its children are `sh`, `cat` and small scripts — never a login shell whose prompt draws itself asynchronously.

**Steps:**

1. Implement `crates/iznik-testkit/src/pty.rs` — `PtyChild::{spawn, write, read_until_quiet, resize, wait}`, `ExitStatus`, `PtyError` — over `portable-pty`, with a reader thread handing chunks over a channel so that `read_until_quiet` can end on silence, exactly as the architecture's `pty-harness` section specifies.
2. Implement `crates/iznik-testkit/src/metrics.rs` — `resident_memory`, `cpu_time`, `MetricsError` — from `/proc`, and write `crates/iznik-testkit/tests/metrics.rs`.
3. Write `crates/iznik-testkit/tests/pty_harness.rs`, every `quiet` and `cap` in it well under a second.

**Tests:**

- Echo round trip: `sh` spawned on the pseudoterminal with `PS1='$ '` echoes a written line, and `read_until_quiet` returns the echo and the prompt and nothing else.
- Quiet, not clock: a child that prints, pauses briefly, then prints more is read as one result when `quiet` exceeds the pause and as two when it does not.
- The cap: a child that never prints produces `PtyError::Timeout` at the cap carrying the escaped bytes received so far, never an empty success.
- Resize is observed: a `sh` script that traps `WINCH` and prints `stty size` reports the new columns and rows after `resize`.
- Exit statuses: a child that exits with a code reports `Exited(code)`; a child killed by a signal reports `Signalled` with the signal's name, never a fake exit code.
- Drop kills: a `PtyChild` dropped while its child runs leaves no process behind, asserted by a census.
- Metrics: `resident_memory` of this process grows by at least the size of a 64 MiB allocation that is touched, `cpu_time` of a child spinning for 100 milliseconds reports at least 50, and an unknown process id is `MetricsError` naming it.

- **Done when:** `timeout 600 cargo nextest run --package iznik-testkit -E 'test(pty_harness) | test(metrics)'` passes every case above and `timeout 3600 cargo xtask check` succeeds.
