# Plan 0006 — Client API and Handoff — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** publish a stable C ABI over `iznik-client` with a byte-pipe surface shaped for libghostty, a golden-tested header and a C smoke program, diagnostics that isolate a fault to one layer, a normative client contract, and a soak that proves the system holds for hours.
- **Root cause:** the macOS application is built separately, in another language, on another machine — so the boundary has to be specified rather than discovered, proven with C rather than promised, and a fault spanning five layers has to be diagnosable from outside all of them.
- **Approach:** treat `docs/CLIENT.md` as the specification the implementation is held to, pin the ABI with a golden header and exercise it from C against a real local daemon reached by a `unix:` alias, ship one command that reports which layer is broken with secrets redacted by construction, and soak before release.
- **Progress:** 2/8 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** — (coordinator-owned blocked/dropped reasons are recorded here).
- **Carried back into 0005:** `pane-byte-pipe` found that a host's task let
  what a host was saying starve the orders already waiting for it, so a burst
  of input went out one to a round trip — a hundred lines took seconds instead
  of milliseconds. The task's loop now takes the orders it has before it hears,
  bounded by `ORDERS_PER_TURN` so a caller who never stops ordering still
  leaves it hearing. Proven by
  `connection_manager_carries_a_burst_as_fast_as_it_is_given`, which fails
  without the fix and passes in a thirtieth of its budget with it.
- **Outcome:** A native application can be built against a written, golden-pinned contract without reading Rust, a C program proves the ABI end to end, any fault in the stack can be isolated to one layer with a single command, and the release checklist has a soak behind it.

_Last updated: 2026-08-29, against `develop` @ `bbf00e4`._
