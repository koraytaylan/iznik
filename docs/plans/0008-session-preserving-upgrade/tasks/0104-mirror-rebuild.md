---
id: mirror-rebuild
title: "Rebuild every adopted pane's mirror so the first screen is exact"
workstream: "0001"
kind: task
depends_on:
  - in-place-replacement
gated: false
touches:
  - crates/iznik-server/src/session/registry.rs
  - crates/iznik-server/src/terminal/mirror.rs
  - crates/iznik-server/tests/session_registry.rs
  - policy/lexicon/mirror-rebuild.txt
  - regression/claims/mirror-rebuild.toml
status: planned
merged_as: ""
---
# Rebuild every adopted pane's mirror so the first screen is exact

A mirror is built from a pane's bytes, so it is rebuilt rather than carried:
the ring crosses, the sequence crosses, and the mirror is fed the ring through
the same path a live pane is, so nothing can disagree about where the stream
is.

**Steps:**

1. `crates/iznik-server/src/session/registry.rs`: `Registry::adopt(state,
   masters, mirror) -> Result<Registry, AdoptError>` building each `Pane` from
   its `AdoptedPane` and feeding its mirror the ring, with `newest` adopted
   verbatim.
2. `crates/iznik-server/src/terminal/mirror.rs`: whatever the mirror needs to
   be built from a byte slice and a starting sequence without a live feed —
   named and confined to the mirror thread's own API.
3. Write the tests in `crates/iznik-server/tests/session_registry.rs`: a pane
   adopted from a ring produces a `Screen` at the carried sequence; a resume
   from a byte inside the ring is contiguous; a ring whose bytes end mid
   escape sequence still produces the screen the emulator would have.
4. Declare the claims in `regression/claims/mirror-rebuild.toml`.

**Tests:**

- An adopted pane answers a `ScreenRequest` at the sequence it carried, and the
  bytes are the emulator's own serialization of that screen.
- A resume from a byte the ring still covers is contiguous with what was there;
  one it does not cover falls back to `Screen`, as always.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test session_registry` passes every case above, `timeout 900 cargo xtask claims verify --task mirror-rebuild` reports every claim proven.
