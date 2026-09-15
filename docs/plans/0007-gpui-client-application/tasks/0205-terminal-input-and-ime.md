---
id: terminal-input-and-ime
title: "Encode input from live terminal mode: keys, paste, mouse, IME, credit"
workstream: "0002"
kind: task
depends_on:
  - terminal-grid-element
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/input.rs
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/tests/input_encoding.rs
  - policy/lexicon/terminal-input.txt
  - regression/claims/terminal-input.toml
status: planned
merged_as: ""
---
# Encode input from live terminal mode: keys, paste, mouse, IME, credit

Make typing correct: key events encoded by the pane's live terminal mode, paste wrapped or not as bracketed paste stands, mouse events in the reported mode, IME preedit rendered at the cursor with the candidate window anchored, and credit returned as snapshots are consumed.

**Steps:**

1. Write `crates/iznik-app/src/input.rs`: key events → byte encodings selected by the emulator's live mode flags (application cursor keys, keypad mode, modify-other-keys); a table-driven mapping documented per row.
2. Paste: bracketed-paste wrapping follows the mode; clipboard content is UTF-8 and bracketed bytes are never interpreted as control input.
3. Mouse: press, drag, release and wheel encoded per the pane's active mouse mode, including SGR encoding; no encoding at all when the program has not asked.
4. IME: render the preedit string inline at the cursor and drive GPUI's IME cursor area so candidate windows anchor to the composition point; commit text flows the key path.
5. Selection copy: serialize the selected range through the emulator so the clipboard holds what the screen shows, styles notwithstanding.
6. Credit: the grid element returns credit for consumed snapshot bytes through the engine, keeping the contract's obligation — a slow surface stalls only its own pane.
7. Write `crates/iznik-app/tests/input_encoding.rs`: the mode-driven encoding table asserted row by row; paste wrapping per mode; mouse encodings per mode; preedit geometry; credit accounting equal to consumption.
8. Declare this task's claims in `regression/claims/terminal-input.toml`.

**Tests:**

- Every mode-flag combination in the table encodes the documented bytes, including the vim-typical application-cursor case.
- Paste in bracketed mode wraps and never executes; outside the mode it sends raw.
- Credit returned equals bytes consumed; a pane that stops consuming is the only one that stops.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task terminal-input` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
