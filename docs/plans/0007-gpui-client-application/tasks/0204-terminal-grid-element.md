---
id: terminal-grid-element
title: "Write the terminal grid element: cells to GPUI, damage, scrollback"
workstream: "0002"
kind: task
depends_on:
  - vt-thread
gated: true
touches:
  - crates/iznik-app/benches/grid_budget.rs
  - crates/iznik-app/src/grid.rs
  - crates/iznik-app/tests/grid_element.rs
  - docs/notes/app-render.md
  - policy/lexicon/terminal-grid.txt
  - regression/claims/terminal-grid.toml
status: planned
merged_as: ""
---
# Write the terminal grid element: cells to GPUI, damage, scrollback

Write the one custom component: a GPUI element that paints cell snapshots — text runs through GPUI's text system, styles, colors, cursor — with damage tracking, a scrollback viewport, and a committed headless render budget. Display-bound frame timings are recorded as deferred, not assumed.

**Steps:**

1. Write `crates/iznik-app/src/grid.rs`: the element consuming a snapshot — cell runs mapped to GPUI text runs with the configured font, styled and colored per cell state, cursor drawn, selection overlay drawn; no second shaping stack and no second glyph atlas.
2. Damage: diff consecutive snapshots per row; repaint only changed rows; an idle pane produces no draw.
3. Scrollback viewport: wheel and keyboard move over the emulator's scrollback extent; new output snaps to bottom only when already at bottom; the sequence tag of the visible frame is carried for assertions.
4. Write `crates/iznik-app/tests/grid_element.rs` in GPUI's test context: build the element from corpus snapshots, assert layout, run-length text mapping, style and color placement, cursor and selection geometry, and that an unchanged snapshot yields no repaint.
5. Write `crates/iznik-app/benches/grid_budget.rs`: snapshot-to-draw-list production over a 10k-cell grid; record the figure and the machine in `docs/notes/app-render.md` with the ceiling the test holds; note display-bound frame timings as deferred following the `darwin-artifacts` pattern.
6. Declare this task's claims in `regression/claims/terminal-grid.toml`.

**Tests:**

- The element renders corpus snapshots without parsing bytes: snapshots are its only input.
- An unchanged snapshot triggers no repaint; a one-row change repaints one row.
- The draw-list budget holds inside its committed ceiling in the benchmark test.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 1200 cargo nextest run --package iznik-app --bench grid_budget --profile regression` prints the budget table inside its ceiling, `timeout 900 cargo xtask claims verify --task terminal-grid` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
