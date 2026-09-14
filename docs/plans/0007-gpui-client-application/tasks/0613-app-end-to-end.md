---
id: app-end-to-end
title: "Prove the app end to end, headless, against the real stack"
workstream: "0006"
kind: task
depends_on:
  - command-palette
  - split-chrome
  - tab-and-session-bars
gated: true
touches:
  - crates/iznik-app/tests/end_to_end.rs
  - docs/notes/app-render.md
  - regression/claims/app-end-to-end.toml
status: planned
merged_as: ""
---
# Prove the app end to end, headless, against the real stack

Prove the application whole: one headless scenario driving the real in-process stack — bootstrap a `unix:` host, open the window in GPUI's test context, create a session from the palette, type into the pane, resize it, split it, drop the link, and resume on the same bytes — asserting what the user would see.

**Steps:**

1. Write `crates/iznik-app/tests/end_to_end.rs`: the application's full assembly (window, bars, palette, grid, bridge, vt thread) against the `iznik-testkit` stack — add host, wait connected, open the palette, dispatch `CreateSession`, dispatch `CreatePane`, observe the grid snapshot show the prompt, send keystrokes through the input path, observe echo, resize, set a split via `SetLayout`, drop the link, observe the banner, resume, observe the screen reset and the uninterrupted stream.
2. Assert the contract's obligations at the surface: screen-before-output on every attach and resume; credit consumed equals credit returned; query answers describe the application's theme.
3. Record the run's shape — events, deltas, snapshots — in `docs/notes/app-render.md` beside the render budget, as the application's committed baseline.
4. Declare this task's claims in `regression/claims/app-end-to-end.toml` as `test` proofs with their `because` — the container adds nothing to a headless window, and the engine's container proofs already exist.

**Tests:**

- The full assembly runs under the standard test deadline: no step waits on wall-clock constants.
- Every assertion reads rendered application state — model mirror, element tree, snapshots — never engine internals.
- The resume path restores pane content without a repaint of unrelated panes.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app --test end_to_end` passes every case above, `timeout 900 cargo xtask claims verify --task app-end-to-end` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
