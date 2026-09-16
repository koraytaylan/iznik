---
id: settings-and-theme
title: "Add settings and theme: one struct of colors, hot-applied"
workstream: "0005"
kind: task
depends_on:
  - action-inventory
  - command-palette
  - terminal-grid-element
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/settings.rs
  - crates/iznik-app/src/theme.rs
  - crates/iznik-app/tests/settings.rs
  - policy/lexicon/settings-theme.txt
  - regression/claims/settings-and-theme.toml
status: done
merged_as: "521b574"
---
# Add settings and theme: one struct of colors, hot-applied

Persist the application's settings — theme, font, keybinding overrides — as one file, apply them hot, and feed the terminal's colors from the same theme struct so a program querying the pane sees this window's truth.

**Steps:**

1. Write `crates/iznik-app/src/theme.rs`: the theme struct — editor colors, ANSI palette, font family and size — mapped to both GPUI theming and the vt thread's emulator colors, so query answers and rendering share one source.
2. Write `crates/iznik-app/src/settings.rs`: the settings file under the platform's configuration directory, loaded at start, watched for change, hot-applied to theme and keybindings; invalid settings refuse with a message naming the field, never a silent default.
3. Keybinding overrides from settings merge over the default keybinding asset the inventory task ships; collisions are refused the same way.
4. Write `crates/iznik-app/tests/settings.rs`: round-trip, hot-apply to a live grid and emulator (a palette change is visible in the next snapshot and in query answers), refusal on malformed files, override merge and collision.
5. Declare this task's claims in `regression/claims/settings-and-theme.toml`.

**Tests:**

- A theme change reaches the emulator's colors: a program asking the background color receives the theme's answer without a restart.
- Malformed settings refuse with the field named and the previous values kept.
- Keybinding overrides resolve against the inventory; an unknown action in settings is a refusal.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task settings-and-theme` reports every claim proven.
