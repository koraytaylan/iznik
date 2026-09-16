# Application rendering baseline

## Headless draw-list budget

Measured on 2026-09-16 on Linux x86_64, AMD Ryzen 9 7950X3D (16 cores,
32 logical CPUs), with the repository's pinned Rust toolchain, default debug
profile and shared Cargo cache. The fixture feeds 120 rows of 100 cells with
alternating ANSI colors through the real client VT thread and converts its
last 100 visible rows into the same draw lists that `TerminalGrid::apply`
uses. Emulator feeding and snapshot extraction are outside the timed region.
Every sample constructs new draw lists; this is not a cached idle frame.

| Workload | Samples | Average | Worst | Committed ceiling |
|---|---:|---:|---:|---:|
| 10,000 scrolling cells, alternating colors | 32 | 1.724 ms | 2.523 ms | 20 ms |

The ceiling guards CPU conversion regressions in an unoptimized build; it
makes no claim about native font shaping, GPU frame time or display refresh.
The same benchmark test is included in the normal integration-test binary
so the workspace and claims gates enforce it. Run its standalone target with:

```sh
timeout 1200 cargo nextest run --package iznik-app --bench grid_budget --success-output immediate
```

## Paint and viewport evidence

Headless tests exercise the actual custom GPUI row element. Cached row
entities reuse their paint subtrees when their draw list is unchanged. The
paint-call proof changes one row while retaining the cursor and observes
counts `[1, 2, 1, 1]`; an identical update changes no counts. Corpus tests
inspect cell-to-run mapping and observed grid bounds after drawing.

History remains on the VT thread. Scroll commands publish the emulator's
viewport offset and cell contents. New output follows the active screen
only when the prior viewport was at the bottom; a scrolled-up viewport keeps
its visible history, subject to the emulator's bounded history retention.

## Display-bound proofs — deferred

- **macOS frame timings: deferred.** No native macOS display measurement has
  been taken. Measure a scrolling 10,000-cell grid with the configured font
  and record hardware, refresh rate, scale factor, median and tail timings.
- **Linux frame timings: deferred.** Headless GPUI tests validate drawing
  commands and caching, not a native GPU/display presentation. Measure on a
  dedicated X11 or Wayland fixture, recording the same metadata.
- Native font fallback, ligature appearance and IME candidate placement need
  visual verification on those displays. CPU run mapping is not evidence of
  their displayed appearance.

Both measurements are declared as `display` records in the claims registry.
They remain deferred even on their named operating system. The verifier does
not run a synthetic test or treat this document as proof of presentation.
The automated scene proofs additionally inspect decoration colors and exact
cell-aligned selection, cursor, inverse-background and underline rectangles.
GPUI wheel and keyboard dispatch is tested independently of display hardware.
