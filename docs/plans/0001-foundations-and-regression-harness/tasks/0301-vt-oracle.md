---
id: vt-oracle
title: "VT Oracle"
workstream: "0003"
kind: task
depends_on:
  - workspace-scaffold
gated: false
touches:
  - "crates/iznik-testkit/src/vt.rs"
  - "crates/iznik-testkit/tests/vt_oracle.rs"
  - "crates/iznik-testkit/tests/fixtures/vt/**"
  - "policy/lexicon/vt-oracle.txt"
status: planned
merged_as: ""
---
# VT Oracle

There is no terminal user interface and there never will be, so every terminal-semantics assertion in this repository runs headless against the emulator the macOS application renders with. The oracle makes "the screen shows X" a byte-exact assertion through the identical engine rather than an opinion.

**Steps:**

1. Confirm against `libghostty-vt` 0.2.1 the calls the oracle is built on — `title()`, `pwd()`, `scrollback_rows()`, `cursor_x()`/`cursor_y()`, `grid_ref` in `Point::Screen` space with `cell()`, `style()` and `graphemes()`, and `CellWide` for continuations — and record in the module documentation, with the crate version, exactly which call backs each field of `Cell` and each accessor of `Vt`.
2. Author the goldens under `crates/iznik-testkit/tests/fixtures/vt/`: for each corpus below, the input bytes and the expected `vt/1` snapshot.
3. Implement `crates/iznik-testkit/src/vt.rs` — `Vt`, `Cell`, `Color`, `Underline`, `Width` and `snapshot` — exactly as the architecture's `vt-oracle` section specifies.
4. Write `crates/iznik-testkit/tests/vt_oracle.rs`.

**Tests:**

- Goldens: plain text; SGR bold, italic, underline styles and 256-color and 24-bit colors; a wide CJK glyph occupying two columns with the second reported as `Continuation`; a line that wraps; cursor movement and erase sequences; alternate screen enter and leave; a title set by OSC 0 and a working directory set by OSC 7; scrollback after more lines than rows; a resize narrower and then wider.
- Determinism: two oracles fed the same bytes produce byte-identical snapshots, and a snapshot contains no pointer, address, capacity or timestamp — asserted by a pattern scan.
- `cell`, `row_text`, `screen_text`, `cursor`, `title`, `working_directory` and `scrollback_rows` agree with the snapshot for every golden.
- A snapshot is versioned `vt/1` on its first line, so a format change is a visible edit to every golden at once.
- An oracle of 200 columns by 50 rows fed 4 MiB of generated text snapshots in under a second, so the flood proofs of later plans are not paying for the instrument.

- **Done when:** `timeout 600 cargo nextest run --package iznik-testkit --test vt_oracle` passes every case above and `timeout 3600 cargo xtask check` succeeds.
