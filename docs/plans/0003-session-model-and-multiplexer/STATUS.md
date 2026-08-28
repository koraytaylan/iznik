# Plan 0003 — Session Model and Multiplexer — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** land the golden-pinned host model, deltas, reconciler and command vocabulary; the server's session registry and command applier; the channel multiplexer with credit-based scheduling; resume with `Screen` as the single resynchronization; and negotiated streaming compression.
- **Root cause:** a client needs identity, arrangement, titles and directories that stay true across reconnects and across other clients, and one link per host needs arbitration so a flooding pane cannot starve a keystroke.
- **Approach:** mint identity once and derive position; emit numbered deltas directly from the operations that cause them and prove convergence by fuzzing with one shared generator; land the channel table, the resume decision and the compression layer as pure pieces before the pump that assembles them; treat each subscription as a cursor into the history ring so the multiplexer buffers nothing; and hold the scheduler to a measured latency figure under a flood.
- **Progress:** 1/10 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** `model-types` added two things to files plan 0001 owns, landed with it and reviewed, because a caller cannot be written against a callee's specified surface without them: `iznik-protocol`'s private `wire` module gained a `Reader::payload` constructor, since a model payload carries no discriminant and `Reader::new` consumes the first byte as one, and a `put_count`/`Reader::count` pair, so the element count every session-model payload carries is written and read in one place; and `MessageError` gained `LayoutTooDeep`, since a recursive-descent decoder over a nesting the wire does not bound is a stack overflow a peer can ask for, and this crate keeps one vocabulary for what a decoder found. `MAXIMUM_LAYOUT_DEPTH` (64), refused by the decoder on the way down and by the encoder and the validator alike, is `model-types`' own addition beyond the architecture's list for that section; the words all of it introduces are in `policy/lexicon/model-types.txt`, attributed to the task that introduced them. It leaves `session-registry` and `session-commands` an obligation: an operation that would nest a layout past the bound is refused with `InvalidLayout`.
- **Outcome:** A server that holds an authoritative, reconcilable host model, answers every command exactly once, carries every subscribed pane over one link with the focused pane responsive under a flood, resumes a client from the byte it holds or the screen it needs, and compresses the link when the numbers say it should.

_Last updated: 2026-08-28, against `develop` @ `bbf00e4`._
