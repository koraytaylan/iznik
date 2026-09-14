---
id: window-shell
title: "Assemble the window: pane grid from the layout tree, resize, focus, banners"
workstream: "0003"
kind: task
depends_on:
  - engine-bridge
  - terminal-input-and-ime
gated: true
touches:
  - crates/iznik-app/README.md
  - crates/iznik-app/src/layout.rs
  - crates/iznik-app/src/window.rs
  - crates/iznik-app/tests/window_shell.rs
  - policy/lexicon/window-shell.txt
  - regression/claims/window-shell.toml
status: planned
merged_as: ""
---
# Assemble the window: pane grid from the layout tree, resize, focus, banners

Build the window shell: the model's layout tree rendered as panes, each pane a grid element attached to its engine subscription; client-owned geometry sent as `Resize`; focus routed to the engine and the scheduler; host state rendered as banners — and every path headless-testable.

**Steps:**

1. Write `crates/iznik-app/src/layout.rs`: the layout tree (`Split` with weights, `Leaf(pane)`) mapped to GPUI dock/resizable-panel layout, per the model deltas the bridge already applies.
2. Write `crates/iznik-app/src/window.rs`: the shell — pane area, attachment lifecycle (subscribe on appearance, unsubscribe on close, re-attach on reconnect with screen-first semantics: reset to the arriving screen before further output), and the resize path that sends `HostManager::resize` with the pane's computed geometry.
3. Focus: keyboard and pointer focus name a pane to `HostManager::focus`; unfocused panes keep rendering from their snapshots.
4. Host state as chrome: connecting, bootstrap progress, failed, upgrading — rendered as a banner over the pane area with the message the engine classifies, and reconnect offered as the action it already is.
5. Write `crates/iznik-app/tests/window_shell.rs`: in-process stack — create session and panes by command, see the tree render, resize a split, observe the delta round-trip, drop the link, observe the banner, observe resume with a screen reset before further output.
6. Declare this task's claims in `regression/claims/window-shell.toml`.

**Tests:**

- Layout tree changes arrive as deltas and the rendered tree matches without rebuilds of unrelated panes.
- After a reconnect the surface receives a screen reset before any further output, per the contract.
- Resize is client-owned: the pane's geometry follows the window, and the last resize wins between surfaces.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task window-shell` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
