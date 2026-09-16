---
id: foreground-cleanup
title: "Repair foreground job cleanup exposed by the application gates"
workstream: "0002"
kind: task
depends_on: []
gated: true
touches:
  - crates/iznik-server/src/pty/spawn.rs
  - crates/iznik-server/src/pane.rs
  - crates/iznik-server/tests/fixtures/foreground.sh
  - crates/iznik-server/tests/fixtures/foreground.rs
  - crates/iznik-server/tests/pty_spawn.rs
  - crates/iznik-server/tests/pane.rs
  - .config/nextest.toml
  - ARCHITECTURE.md
  - docs/plans/0002-server-core-panes-and-terminals/ARCHITECTURE.md
  - policy/lexicon/foreground-cleanup.txt
  - regression/claims/foreground-cleanup.toml
status: done
merged_as: ""
---
# Repair foreground job cleanup exposed by the application gates

The application gate intermittently fails `pane_exits_and_leaves_nothing`.
On 2026-09-16 the failing shell PID 1388489 had a surviving foreground job
PID/process-group 1388492 in session 1388489. The test waited 25 seconds for
the shell to be reaped while the job kept the terminal open. The leftover job
was identified by its session and explicitly cleaned up. An isolated rerun
and the following workspace run passed, demonstrating the scheduling race.

The assumption that an interactive shell's foreground job remains in the
shell's process group is false. The existing nextest scheduling override
cannot repair this and its explanation incorrectly attributes the leak to
load. This correction is needed for reliable verification and pane ownership.

**Steps:**

1. Commit a deterministic foreground fixture that announces readiness from
   the child itself and ignores hangup, plus a kernel proof that its group is
   distinct from the shell's but belongs to the same terminal session.
2. Centralize forced cleanup of the terminal's current foreground group and
   the owned shell group in the PTY layer. Use the safe portable-pty foreground
   query; never signal zero or an unrelated group. Preserve the reaped guard.
3. Use that cleanup for direct PTY drop, pane drop and close escalation. A
   pane drop must not wait for the reaper's process mutex: move its blocking
   wait onto a PID/completion handle while retaining the PTY owner. Hang up
   the foreground first and keep the shell session alive during the configurable
   grace period so forced cleanup can still query an ignoring job.
4. Replace the command-start mark as evidence of foreground readiness with
   the child fixture's own mark. Assert termination of both shell and job.
5. Remove the misleading test scheduling workaround and document the Unix
   boundary: detached jobs that leave the owned groups are outside group
   signaling, while the terminal's ordinary foreground job is included.

**Tests:**

- The committed fixture establishes a distinct foreground group in the owned
  session, with explicit cleanup even before the product repair.
- Dropping a direct PTY terminates the ready foreground job and the shell.
- Dropping a pane terminates both, reaps the shell and finishes promptly.
- Close escalation terminates a hangup-ignoring foreground job and reports
  the shell's exit without waiting for the fixture sleep to expire.
- Closing remains possible after a child closes its terminal descriptors but
  keeps running, proving that the child wait does not block terminal access.
- Existing PTY and pane cases remain passing, without slow in-process tests.

**Done when:** `timeout 600 cargo nextest run --package iznik-server --test
pty_spawn --test pane`, `timeout 900 cargo xtask claims verify --task
foreground-cleanup`, and `timeout 3600 cargo xtask check` all pass.

## Verification of the failure mechanism

With the repaired source saved and restored automatically, removing only the
foreground-group kill makes `pty_spawn_drop_kills_the_foreground_job` fail at
its two-second census deadline. The fixture's verified-session cleanup then
terminates the leftover job. With foreground cleanup restored, the case
finishes in milliseconds. This distinguishes the ownership repair from a
scheduling workaround or a test that merely waits longer.

## Acceptance

All 23 PTY/pane tests pass, and all five `foreground-cleanup` claims are
proven. The workspace gates pass with 517 tests and 49 branch claims proven;
the two deferred display measurements belong to the application grid, not
this correction. The foreground lifecycle cases complete in milliseconds.
The old exclusive scheduling override is removed.
