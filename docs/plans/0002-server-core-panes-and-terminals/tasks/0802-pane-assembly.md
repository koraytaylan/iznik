---
id: pane-assembly
title: "Pane Assembly"
workstream: "0008"
kind: task
depends_on:
  - pty-streams
  - screen-serializer
  - shell-integration-marks
  - history-ring
gated: false
touches:
  - "crates/iznik-server/src/pane.rs"
  - "crates/iznik-server/tests/pane.rs"
  - "regression/claims/pane-assembly.toml"
  - "policy/lexicon/pane-assembly.txt"
status: planned
merged_as: ""
---
# Pane Assembly

Process, streams, mirror, history and observer behind one interface, with the pane's VT task on the mirror thread and everything else free to run anywhere. The design decision this task realizes is that the ring is the queue: a subscriber learns that new bytes exist and reads them at its own cursor, and nothing is ever pushed to a subscriber that cannot take it.

**Steps:**

1. Implement `crates/iznik-server/src/pane.rs` — `Pane`, `PaneState`, `PaneError`, the VT task shipped to the `MirrorThread`, the alternate-screen split around the observer's events, `read_history`, the subscriber-count handling of the mirror's pending responses — exactly as the architecture's `pane-assembly` section specifies.
2. Write `crates/iznik-server/tests/pane.rs`, its shells `bash --rcfile` with the testkit's shell-integration asset and its floods from `iznik_testkit::corpus::generated`.
3. Declare this task's claims in `regression/claims/pane-assembly.toml` as `test` proofs with their `because`; the same behavior through the static binary is proven by `fidelity-suite`.

**Tests:**

- Round trip through the screen: `input("echo hello-from-iznik\n")`, then after the `CommandFinished` mark, `screen()` fed into a `Vt` shows the echoed line.
- History is the stream: the bytes a `cat` of a generated 4 MiB file produced are byte-identical in `read_history` from sequence zero, and `state()` published a `newest` equal to their length.
- Sequences are exact: `screen().sequence` equals `state().newest` at the moment of the call, and bytes appended afterwards start at that sequence.
- Response policy end to end: a script that sends a cursor position query and reads the reply prints the reply with zero subscribers and prints nothing after `subscribe()`.
- Marks flow: the four OSC 133 events and the OSC 7 event from the integration shell arrive on `marks()` with the expected kinds.
- Alternate screen: a script that prints a line, enters the alternate screen with `1049`, prints another, and stops — `screen()` reproduces through the oracle as the alternate content with the primary line revealed on leaving.
- Resize is observed by the child and reflected in `state()`.
- Exit: after `close()`, `exit_status()` resolves, `state().exited` is set, and the pane's task ends; dropping a `Pane` whose child runs leaves no process.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test pane` passes every case above, `timeout 900 cargo xtask claims verify --task pane-assembly` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
