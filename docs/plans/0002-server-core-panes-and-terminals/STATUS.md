# Plan 0002 — Server Core: Panes and Terminals — ✅ Done

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** ✅ Done.

- **Goal:** land the anatomy of a pane — a pseudoterminal with a login shell, an async output stream and an atomic input path, a `libghostty-vt` mirror with a screen serializer and a query-response policy, a shell-integration observer, and a bounded history ring — assembled behind one interface and proven byte-exact inside the host container.
- **Root cause:** the server iznik needs is a pseudoterminal holder that can say what a pane looks like right now; everything a multiplexer adds between the shell and the surface is something to work around.
- **Approach:** own the pseudoterminal through `portable-pty` with one named module for the blocking bridge, mirror every byte in the emulator the client renders with — on the one thread its handle can live on — so a `Screen` is exact by construction, remember the primary screen when a program leaves it, keep the last bytes in a ring so a short drop costs nothing, and hold the whole path to the fidelity corpus through the static binary.
- **Progress:** 9/9 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** — (coordinator-owned blocked/dropped reasons are recorded here).
  Plan 0005's work found `screen-serializer` a defect, recorded here because the file is this plan's. A cursor resting on the last column carries a wrap that `CUP` cannot say: a program that has just filled a line leaves the cursor there with the next character bound for the next row, and an absolute position reproduced the place without the pending wrap. A client that attached at exactly that moment rendered one column to the left from then on, for as long as the program went on printing — one character in every eighty for a pane pouring full-width lines, which is why the fidelity corpus, whose screens rest at a prompt, never met it. The serializer writes the last cell's own text at that column now rather than moving onto it, which is what puts a terminal into the state, and the character written is the one already there. Found by `attachment_is_exact_under_load` of plan 0003, held by `screen_serializer_keeps_a_pending_wrap`, and recorded in `docs/notes/terminal-mirror.md`.

- **Outcome:** A `Pane` that spawns a login shell, streams its bytes with contiguous absolute sequence numbers, answers queries only when nobody is attached, serializes its screen for an exact cold attach, emits shell-integration marks, and is proven byte-identical across the fidelity corpus and a flood inside the host container.

_Last updated: 2026-08-28, against `develop` @ `e67b314`._
