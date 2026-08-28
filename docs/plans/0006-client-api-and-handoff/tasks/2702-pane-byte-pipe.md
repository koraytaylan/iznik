---
id: pane-byte-pipe
title: "Pane Byte Pipe"
workstream: "0027"
kind: task
depends_on:
  - ffi-surface
gated: false
touches:
  - "crates/iznik-ffi/src/pane.rs"
  - "crates/iznik-ffi/tests/pane_byte_pipe.rs"
  - "regression/claims/pane-byte-pipe.toml"
  - "policy/lexicon/pane-byte-pipe.txt"
status: done
merged_as: ""
---
# Pane Byte Pipe

The one path in the ABI where bytes are borrowed rather than copied, shaped for direct handoff into a libghostty surface, with credit the application must return, a screen callback it must obey, and input that carries its emulator's query responses like keystrokes.

**Steps:**

1. Implement `crates/iznik-ffi/src/pane.rs` — `iznik_pane_attach`, `iznik_pane_detach`, `iznik_pane_callbacks`, `iznik_pane_credit`, `iznik_pane_input`, `iznik_pane_resize`, `iznik_pane_focus` — exactly as the architecture's `pane-byte-pipe` section specifies, with their `Obligation:` lines.
2. Write `crates/iznik-ffi/tests/pane_byte_pipe.rs` against an in-process `Stack` through a `unix:` alias.
3. Declare this task's claims in `regression/claims/pane-byte-pipe.toml` as `test` proofs with their `because`.

**Tests:**

- Attach: the `screen` callback arrives first with the pane's size and a screen that reproduces through the oracle, then `output` callbacks whose concatenation is the pane's bytes from that sequence.
- Borrow, not copy: the `output` callback's pointer is into the frame buffer, asserted by pointer identity against the received frame.
- Credit: without returned credit the output stops after the initial window; returning credit resumes it; a second attached pane keeps flowing throughout.
- Input atomicity: a hundred concurrent `iznik_pane_input` calls with distinct patterns arrive contiguous in the shell's echo.
- Resize: after `iznik_pane_resize`, a `PaneResized` delta arrives on the event callback and the shell's `stty size` agrees.
- Screen obligation: after a forced screen — a stale background pane refocused — the `screen` callback precedes any further `output`.
- Detach: after `iznik_pane_detach` no further callback arrives for the pane, and the channel is released, asserted by a subsequent attach receiving channel 1 again.

- **Done when:** `timeout 600 cargo nextest run --package iznik-ffi --test pane_byte_pipe` passes every case above, `timeout 900 cargo xtask claims verify --task pane-byte-pipe` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
