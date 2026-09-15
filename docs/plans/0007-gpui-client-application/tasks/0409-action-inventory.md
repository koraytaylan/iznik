---
id: action-inventory
title: "Build the closed action inventory with explanations and keybindings"
workstream: "0004"
kind: task
depends_on:
  - split-chrome
  - window-shell
gated: false
touches:
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/assets/default-keybindings.json
  - crates/iznik-app/src/actions.rs
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/tests/inventory.rs
  - policy/lexicon/action-inventory.txt
  - regression/claims/action-inventory.toml
status: planned
merged_as: ""
---
# Build the closed action inventory with explanations and keybindings

Enumerate everything the application can do as GPUI actions in one registry: one action per protocol session command, one per `HostManager` capability, each with its explanation lifted from the command documentation, its engine target, and its availability predicate; bind the default keybindings over the same table.

**Steps:**

1. Write `crates/iznik-app/src/actions.rs`: the action enum and the registry table — display name, explanation, engine call, availability predicate, whether it needs a pane/session/host context — with the explanations sourced from the protocol command documentation, not rewritten.
2. Availability: predicates over the model mirror and host states ("New Pane" needs a tab; "Reconnect" needs a failed or disconnected host; "Upgrade" needs an offered upgrade), evaluated per focused context.
3. Write `crates/iznik-app/assets/default-keybindings.json` binding a default chord to every action the palette will list; unbound actions are listed in the palette with no chord, never silently.
4. Write `crates/iznik-app/tests/inventory.rs`: the inventory covers every variant of the protocol's session command enum and every public `HostManager` method — a new engine capability fails this test until it is registered with an explanation; no explanation duplicates the docs it lifts; every keybinding names a registered action.
5. Declare this task's claims in `regression/claims/action-inventory.toml`.

**Tests:**

- The inventory is closed: protocol commands and engine methods not in it fail the test.
- Every action carries a non-empty explanation and an engine target that exists.
- Every default chord resolves to exactly one action; no two chords collide.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task action-inventory` reports every claim proven.
