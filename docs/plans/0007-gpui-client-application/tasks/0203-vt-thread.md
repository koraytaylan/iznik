---
id: vt-thread
title: "Run client-side libghostty-vt terminals on one dedicated thread"
workstream: "0002"
kind: task
depends_on:
  - engine-bridge
  - gpui-adoption
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/vt.rs
  - crates/iznik-app/tests/vt_thread.rs
  - policy/lexicon/vt-thread-client.txt
  - regression/claims/vt-thread.toml
status: planned
merged_as: ""
---
# Run client-side libghostty-vt terminals on one dedicated thread

Stand up the client's emulator service: one dedicated thread running a `LocalSet` that owns every pane's `libghostty-vt` terminal — the server's mirror-thread pattern, client-side — producing sequence-tagged cell snapshots for the grid and query responses for the pane.

**Steps:**

1. Write `crates/iznik-app/src/vt.rs`: the thread, its `LocalSet`, and the handle set — feed bytes, resize, set theme colors, request snapshot, close — with the thread owning every terminal and no other thread touching a handle.
2. Feed pane output from the engine's subscribe path into the emulator; on each batch, produce a cell snapshot (text runs, styles, colors, cursor, alternate-screen state, scrollback extent) tagged with the pane's absolute sequence, and deliver it on the snapshot channel.
3. Capture the emulator's query responses (`on_pty_write` composition and the dedicated effects the terminal-mirror note records) and forward them to the engine as pane input, exactly as the server's unattended path does in reverse.
4. Apply the theme: the emulator's palette and background come from the application's theme struct, so a program's color queries describe this window.
5. Write `crates/iznik-app/tests/vt_thread.rs`: feed the fidelity corpus's terminal output through the handle and assert snapshots against the VT oracle's expected state; assert query responses are composed for an unattached pane and suppressed once the application's emulator answers instead; assert resize and alternate-screen transitions.
6. Declare this task's claims in `regression/claims/vt-thread.toml`.

**Tests:**

- Corpus-driven snapshots match the VT oracle's rendering of the same bytes.
- A pane with no attached emulator on the client side would hang a querying program; the service proves responses flow and stop when the application answers instead.
- Snapshots carry contiguous sequence tags; a gap is an error the caller resolves by requesting a fresh screen, never by guessing.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task vt-thread` reports every claim proven.
