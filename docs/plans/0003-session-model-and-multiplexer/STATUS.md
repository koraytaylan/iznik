# Plan 0003 — Session Model and Multiplexer — 📋 Planned

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 📋 Planned.

- **Goal:** land the golden-pinned host model, deltas, reconciler and command vocabulary; the server's session registry and command applier; the channel multiplexer with credit-based scheduling; resume with `Screen` as the single resynchronization; and negotiated streaming compression.
- **Root cause:** a client needs identity, arrangement, titles and directories that stay true across reconnects and across other clients, and one link per host needs arbitration so a flooding pane cannot starve a keystroke.
- **Approach:** mint identity once and derive position; emit numbered deltas directly from the operations that cause them and prove convergence by fuzzing with one shared generator; land the channel table, the resume decision and the compression layer as pure pieces before the pump that assembles them; treat each subscription as a cursor into the history ring so the multiplexer buffers nothing; and hold the scheduler to a measured latency figure under a flood.
- **Progress:** 0/10 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** — (coordinator-owned blocked/dropped reasons are recorded here).
- **Outcome:** A server that holds an authoritative, reconcilable host model, answers every command exactly once, carries every subscribed pane over one link with the focused pane responsive under a flood, resumes a client from the byte it holds or the screen it needs, and compresses the link when the numbers say it should.

_Last updated: 2026-08-26, against `develop` @ `2cc3de3`._
