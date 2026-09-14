---
id: tab-and-session-bars
title: "Render the two bars: tabs at the top, sessions at the bottom"
workstream: "0003"
kind: task
depends_on:
  - window-shell
gated: true
touches:
  - crates/iznik-app/README.md
  - crates/iznik-app/src/bars.rs
  - crates/iznik-app/tests/bars.rs
  - policy/lexicon/tab-bars.txt
  - regression/claims/tab-and-session-bars.toml
status: planned
merged_as: ""
---
# Render the two bars: tabs at the top, sessions at the bottom

Compose the Rune-style chrome from kit components, driven entirely by the model: a tab bar along the top for the focused session's tabs, a session bar along the bottom for every host's sessions with connection state, and switching, closing and overflow as keyboard-first operations.

**Steps:**

1. Write `crates/iznik-app/src/bars.rs`: the top bar — one entry per tab of the focused session, title, activity badge from marks (command running, failed exit), close affordance, overflow into a scrollable strip.
2. The bottom bar — one entry per session grouped by host, the host's connection state as a dot, rename inline, close, and the palette as the fallback for everything the bars do not show.
3. Switching: click and keyboard next/previous; the focus path from the input task names the newly visible pane.
4. Empty states: no hosts (the add-host action front and center), no sessions, host failed — each a rendered state, never a blank bar.
5. Write `crates/iznik-app/tests/bars.rs`: bars track snapshots and deltas exactly; switching updates focus and the grid; closing sends the command and reconciles on the delta; the three empty states render.
6. Declare this task's claims in `regression/claims/tab-and-session-bars.toml`.

**Tests:**

- Bars render only model state: no local copy of what the host has said, optimistic effects included through the reducer as the engine defines them.
- Closing a tab with one pane closes the pane; the exit-status delta removes the entry.
- A failed host marks its sessions' entries and the banner without touching other hosts' bars.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task tab-and-session-bars` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
