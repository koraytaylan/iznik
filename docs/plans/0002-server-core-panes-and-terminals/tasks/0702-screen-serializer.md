---
id: screen-serializer
title: "Screen Serializer"
workstream: "0007"
kind: task
depends_on:
  - terminal-mirror
gated: false
touches:
  - "crates/iznik-server/src/terminal/screen.rs"
  - "crates/iznik-server/tests/screen_serializer.rs"
  - "regression/claims/screen-serializer.toml"
  - "policy/lexicon/screen-serializer.txt"
status: planned
merged_as: ""
---
# Screen Serializer

The single resynchronization mechanism in the protocol: the mirror's state as VT sequences, exact at a named sequence number. Its acceptance is a property, not an example — feed the output into a fresh emulator and get the mirror back, scrollback and cursor included — and because the emulator cannot format an inactive screen, the primary screen is remembered at the moment a program leaves it.

**Steps:**

1. Confirm against `libghostty-vt` 0.2.1 that the formatter with no selection emits the scrollback and that a selection starting lower drops the oldest rows, and record the finding in the module documentation.
2. Implement `crates/iznik-server/src/terminal/screen.rs` — `serialize`, `SerializedScreen`, `ScreenState` with `entering_alternate`, `leaving_alternate` and its own `serialize`, `ScreenError`, `MAXIMUM_SCREEN_BYTES` — exactly as the architecture's `screen-serializer` section specifies, palette not emitted.
3. Write `crates/iznik-server/tests/screen_serializer.rs`.
4. Declare this task's claims in `regression/claims/screen-serializer.toml` as `test` proofs with their `because`.

**Tests:**

- The property: for every construct in the fidelity corpus and for five hundred generated sequences of up to a hundred text, SGR, cursor movement, erase, scroll and resize operations over a 40-column, 12-row terminal with scrollback, feeding the serialized bytes into a fresh `Vt` of the same size yields a snapshot equal to a `Vt` fed the original bytes — including scrollback rows and cursor position — in under five seconds all told.
- Alternate screen: with a `ScreenState` told about the switch, a corpus that enters the alternate screen serializes as the remembered primary, the switch, and the alternate screen; leaving the alternate screen in the reproduction reveals the primary content; after `leaving_alternate` a fresh serialization holds only the primary.
- Styles survive: bold, italic, underline styles, 256-color and 24-bit colors, and hyperlinks are present in the reproduction's cells.
- No palette: the output contains no OSC 4 sequence.
- The bound: a mirror with more scrollback than fits serializes to at most `MAXIMUM_SCREEN_BYTES`, reports `dropped_rows`, and reproduces the newest rows exactly.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test screen_serializer` passes every case above, `timeout 900 cargo xtask claims verify --task screen-serializer` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
