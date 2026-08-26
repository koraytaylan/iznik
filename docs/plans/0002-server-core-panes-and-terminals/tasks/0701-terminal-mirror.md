---
id: terminal-mirror
title: "Terminal Mirror"
workstream: "0007"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-server/src/terminal/mod.rs"
  - "crates/iznik-server/src/terminal/mirror.rs"
  - "crates/iznik-server/tests/terminal_mirror.rs"
  - "regression/claims/terminal-mirror.toml"
  - "policy/lexicon/terminal-mirror.txt"
status: planned
merged_as: ""
---
# Terminal Mirror

Beside every pseudoterminal runs a `libghostty-vt` terminal fed every byte — the same engine the macOS application renders with. It is how the server knows what a pane looks like, and it is where a program's queries are answered when nobody is attached and left to the real terminal when somebody is. Because the emulator's handle cannot cross threads, this task also lands the one thread every mirror lives on.

**Steps:**

1. Confirm against `libghostty-vt` 0.2.1 which queries arrive through `on_pty_write` and which through `on_device_attributes`, `on_xtversion`, `on_enquiry`, `on_size` and `on_color_scheme`, by feeding each query the fidelity corpus contains and recording which effect fired, and write the finding into the module documentation with the crate version.
2. Implement `crates/iznik-server/src/terminal/mod.rs` and `mirror.rs` — `MirrorThread` with its constructor-shipping `spawn`, `Mirror`, `MirrorError`, `MIRROR_SCROLLBACK_ROWS`, the response policy through `set_subscriber_count` and `take_pending_responses`, and the named constants every embedder-side answer is composed from — exactly as the architecture's `terminal-mirror` section specifies.
3. Write `crates/iznik-server/tests/terminal_mirror.rs`.
4. Declare this task's claims in `regression/claims/terminal-mirror.toml` as `test` proofs with their `because`.

**Tests:**

- The thread: a mirror created through `MirrorThread::spawn` from a multi-threaded runtime is fed and read from its task; two panes' tasks interleave — a task feeding 4 MiB in 64 KiB chunks does not delay a neighbor's single-chunk feed by more than a stated bound; dropping the `MirrorThread` ends its tasks.
- Agreement with the oracle: for every construct in the fidelity corpus, the mirror's title, working directory, dimensions and alternate-screen flag match the oracle's after the same bytes and the same resizes.
- Cursor position report: with zero subscribers, feeding `ESC [ 6 n` yields a `CPR` response in `take_pending_responses`; with one subscriber, nothing is pending.
- Every embedder-side query — primary and secondary device attributes, XTVERSION, ENQ, a size report, a color-scheme query — yields the named constant's answer with zero subscribers and nothing with one.
- Ignored effects: a bell, an OSC 52 write, a title and a directory report produce no pending bytes and no error.
- Scrollback is bounded at `MIRROR_SCROLLBACK_ROWS` after more lines than that.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test terminal_mirror` passes every case above, `timeout 900 cargo xtask claims verify --task terminal-mirror` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
