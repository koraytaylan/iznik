---
id: terminal-grid-element
title: "Write the terminal grid element: cells to GPUI, damage, scrollback"
workstream: "0002"
kind: task
depends_on:
  - vt-thread
gated: true
touches:
  - CONTRIBUTING.md
  - xtask/src/claims/registry.rs
  - xtask/src/claims/verify.rs
  - xtask/tests/claims.rs
  - xtask/README.md
  - crates/iznik-app/tests/support/mod.rs
  - crates/iznik-app/tests/vt_thread.rs
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/vt.rs
  - crates/iznik-app/src/grid/paint.rs
  - crates/iznik-app/benches/grid_budget.rs
  - crates/iznik-app/src/grid/mod.rs
  - crates/iznik-app/tests/grid_element.rs
  - docs/notes/app-render.md
  - policy/lexicon/terminal-grid-element.txt
  - regression/claims/terminal-grid-element.toml
status: done
merged_as: ""
---
# Write the terminal grid element: cells to GPUI, damage, scrollback

Write the one custom component: a GPUI element that paints cell snapshots — text runs through GPUI's text system, styles, colors, cursor — with damage tracking, a scrollback viewport, and a committed headless render budget. Display-bound frame timings are recorded as deferred, not assumed.

**Steps:**

1. Write `crates/iznik-app/src/grid/mod.rs`: the element consuming a snapshot — cell runs mapped to GPUI text runs with the configured font, styled and colored per cell state, cursor drawn, selection overlay drawn; no second shaping stack and no second glyph atlas.
2. Damage: diff consecutive snapshots per row; repaint only changed rows; an idle pane produces no draw.
3. Scrollback viewport: wheel and keyboard move over the emulator's scrollback extent; new output snaps to bottom only when already at bottom; the sequence tag of the visible frame is carried for assertions.
4. Write `crates/iznik-app/tests/grid_element.rs` in GPUI's test context: build the element from corpus snapshots, assert layout, run-length text mapping, style and color placement, cursor and selection geometry, and that an unchanged snapshot yields no repaint.
5. Write `crates/iznik-app/benches/grid_budget.rs`: snapshot-to-draw-list production over a 10k-cell grid; record the figure and the machine in `docs/notes/app-render.md` with the ceiling the test holds; note display-bound frame timings as deferred following the `darwin-artifacts` pattern.
6. Declare this task's claims in `regression/claims/terminal-grid-element.toml`.

**Tests:**

- The element renders corpus snapshots without parsing bytes: snapshots are its only input.
- An unchanged snapshot triggers no repaint; a one-row change repaints one row.
- The draw-list budget holds inside its committed ceiling in the benchmark test.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 1200 cargo nextest run --package iznik-app --bench grid_budget --success-output immediate` prints the budget table inside its ceiling, `timeout 900 cargo xtask claims verify --task terminal-grid-element` reports every automated claim proven and both display measurements deferred, and `timeout 3600 cargo xtask check` succeeds.

## Wiring and runner corrections

The renderer must request viewport snapshots from the VT owner; the existing
snapshot only exposes active-screen rows and its history extent. This task
therefore owns the VT scroll command and viewport metadata as well as crate
module registration. Painting lives in a private `grid/paint.rs` module so
geometry, update policy and framework drawing each remain reviewable under
the file and function limits. GPUI cached row entities reuse unchanged paint
subtrees using the framework's public API; an idle update does not notify.

The vocabulary and claims filenames follow the actual task id. There is no
nextest `regression` profile in this repository. The headless budget is a
short in-process test in the bench target, run with the default deadline and
`--success-output immediate` to show its measured table. Display timings stay
explicitly deferred. The existing committed fidelity corpus is reused.

The existing VT fixture helpers move into `tests/support/mod.rs` so corpus,
grid and budget tests share one bounded wait implementation.

The grid root uses `grid/mod.rs`, as required by the workspace lint when a
module has child files. Pixel arithmetic converts to finite floating-point
coordinates with explicit saturation before constructing framework pixels.

## Acceptance evidence — 2026-09-16

All 19 application tests pass. The standalone 10,000-cell benchmark measured
1.724 ms average and 2.523 ms worst over 32 samples against the 20 ms ceiling.
The task's claims report 11 proven, zero missing or failed, and the two native
display measurements explicitly deferred. All five workspace gates pass:
495 tests, no slow tests, 26 branch claims proven and two deferred.

The headless proofs cover the corpus, cached row paints, owned scrollback,
GPUI keyboard/wheel dispatch, submitted decoration colors and geometry,
selection, cursor, inverse backgrounds, and display-registry validation.
Native font appearance and presentation timing remain the documented display
measurements; no headless test is presented as proof of those properties.

## Display proof correction

The existing `platform` filter cannot represent a manual display measurement
on the same operating system as the headless gate. Add a mutually exclusive
`display = "docs/notes/app-render.md"` proof category with a nonempty `because`.
The loader checks a normal relative Markdown path under `docs/notes/` and an
existing file. The verifier schedules no test for this category and always
returns deferred, with the reason and record path, even if a supplied test
report contains a similarly named passing case. Preserve existing scenario,
test, platform and cargo-profile behavior. Synthetic registry tests cover
exclusive proof selection, absent reasons, invalid or missing records, and
non-execution and deferral on the running platform. The rule is documented
for every task in the contributing policy in its own commit.

The wheel/keyboard, submitted-scene decoration/overlay, and registry proofs
pass. Both display measurement claims are registered and remain deferred.
