---
id: split-chrome
title: "Wire split chrome: divider drag to SetLayout, keyboard splits"
workstream: "0003"
kind: task
depends_on:
  - tab-and-session-bars
  - window-shell
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/splits.rs
  - crates/iznik-app/tests/splits.rs
  - policy/lexicon/split-chrome.txt
  - regression/claims/split-chrome.toml
status: planned
merged_as: ""
---
# Wire split chrome: divider drag to SetLayout, keyboard splits

Make splits interactive: dividers drag to reweight through `SetLayout`, split and move commands arrive from keyboard and palette, and the pane geometry the client owns follows the authoritative delta rather than the drag.

**Steps:**

1. Write `crates/iznik-app/src/splits.rs`: divider hit zones over the layout tree; a drag maps pointer motion to integer weights and sends `SetLayout` through the command path; the rendered tree follows the returned delta, so a refused or coalesced drag leaves the layout the host holds.
2. Keyboard: split right/below, move pane, equalize — actions in the inventory (the palette task consumes them), each naming stable identity per the session-command rules.
3. Geometry: after a layout delta, each pane's size is recomputed from the tree and sent as `Resize`; the drag preview is local, the authority is the delta.
4. Write `crates/iznik-app/tests/splits.rs`: drag produces a `SetLayout` whose weights match the pointer within tolerance; the delta re-renders; a refused command reverts the preview; resize follows; equalize restores shared factors.
5. Declare this task's claims in `regression/claims/split-chrome.toml`.

**Tests:**

- The rendered arrangement always matches the host's normalized tree, including the weight-reduction rule the protocol tests pin.
- A drag that ends where it started sends nothing.
- Pane resize after re-layout keeps the focused pane's geometry authoritative and observed by every attached surface.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task split-chrome` reports every claim proven.
