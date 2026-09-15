---
id: command-palette
title: "Compose the command palette: fuzzy filter, explanations, dispatch"
workstream: "0004"
kind: task
depends_on:
  - action-inventory
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/palette.rs
  - crates/iznik-app/tests/palette.rs
  - policy/lexicon/command-palette-app.txt
  - regression/claims/command-palette.toml
status: planned
merged_as: ""
---
# Compose the command palette: fuzzy filter, explanations, dispatch

Render the registry as the palette: gpui-component's command palette overlay, filtered over the action inventory, showing each entry's explanation, honoring availability, and dispatching through the actions so palette, chords and keybindings are one surface.

**Steps:**

1. Write `crates/iznik-app/src/palette.rs`: the overlay over the terminal window — dimmed, centered, focus-trapped — listing available inventory entries with their explanations; fuzzy filter as you type; arrow navigation; enter dispatches the action; escape closes.
2. Scoping: entries evaluate availability against the focused host, session and pane; a host-scoped entry names its host when several exist.
3. Results: command results and refusals surface as notifications in the shell, not inside the palette; the palette closes on dispatch.
4. Write `crates/iznik-app/tests/palette.rs` in GPUI's test context: open by action, filter to one entry, dispatch a session command against the in-process stack, see the delta land and the notification appear; an unavailable entry never lists; a refused command's notification names the refusal.
5. Declare this task's claims in `regression/claims/command-palette.toml`.

**Tests:**

- The palette lists exactly the available inventory, no more, and unavailable entries never appear.
- Dispatch is the action path: a command fired from the palette is indistinguishable on the wire from its keybinding.
- Explanations render from the registry — the palette adds no text of its own.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task command-palette` reports every claim proven.
