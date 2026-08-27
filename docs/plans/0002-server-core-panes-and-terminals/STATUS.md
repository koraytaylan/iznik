# Plan 0002 — Server Core: Panes and Terminals — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** land the anatomy of a pane — a pseudoterminal with a login shell, an async output stream and an atomic input path, a `libghostty-vt` mirror with a screen serializer and a query-response policy, a shell-integration observer, and a bounded history ring — assembled behind one interface and proven byte-exact inside the host container.
- **Root cause:** the server iznik needs is a pseudoterminal holder that can say what a pane looks like right now; everything a multiplexer adds between the shell and the surface is something to work around.
- **Approach:** own the pseudoterminal through `portable-pty` with one named module for the blocking bridge, mirror every byte in the emulator the client renders with — on the one thread its handle can live on — so a `Screen` is exact by construction, remember the primary screen when a program leaves it, keep the last bytes in a ring so a short drop costs nothing, and hold the whole path to the fidelity corpus through the static binary.
- **Progress:** 4/9 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** — (coordinator-owned blocked/dropped reasons are recorded here).
- **Outcome:** A `Pane` that spawns a login shell, streams its bytes with contiguous absolute sequence numbers, answers queries only when nobody is attached, serializes its screen for an exact cold attach, emits shell-integration marks, and is proven byte-identical across the fidelity corpus and a flood inside the host container.

_Last updated: 2026-08-27, against `develop`._
