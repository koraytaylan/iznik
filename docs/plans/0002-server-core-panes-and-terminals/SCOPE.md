# Scope — Plan 0002

> Own the pseudoterminal, mirror it in the same emulator the client renders with, remember its bytes, and prove that what a program writes is what a client receives — byte for byte, through the static binary the bootstrap will ship.

## Why this plan

The previous incarnation of this project spent two plans teaching a third-party multiplexer to hand over a pane's bytes. The server iznik actually needs holds the pseudoterminal itself, and this plan builds that: a child process on a real pseudoterminal with a controlling terminal and a login shell, an output stream that never blocks anything but its own child, an input path that never interleaves one writer's bytes with another's and never silently discards a paste.

Beside each pseudoterminal runs a terminal mirror — a `libghostty-vt` terminal fed every byte. It is the answer to the question every remote terminal has to answer on reconnect: what does this pane look like now? Replaying history into a freshly created surface is inexact the moment the ring has aged out or the size has changed; formatting the mirror's state as VT sequences is exact by construction, because the client applies it in the identical emulator. The mirror is also where the server learns titles and working directories and answers a program's queries when nobody is attached.

The history ring makes the common case — a link that drops for seconds — invisible: a reconnecting client names the byte it holds and receives exactly the bytes it missed. And the fidelity suite is the argument for the whole architecture: Kitty graphics, hyperlinks, clipboard writes, prompt marks, sequences split across reads, wide glyphs, a lone escape at a buffer boundary and a multi-gigabyte flood all arrive byte-identical, proven inside the host container against the musl binary, not on a developer's machine.

## In scope

- **0006 — Pseudoterminal Ownership.** Spawning the user's login shell on a pseudoterminal in its own session with the environment a ghostty-rendered terminal deserves, faithful exit statuses including signal deaths, resize, and the one module where a blocking descriptor becomes an async stream: an output stream and an input path with whole-message atomicity and an admitted backlog instead of a silent drop.
- **0007 — Terminal Mirror.** The mirror thread and the `libghostty-vt` mirror with its query-response policy, the screen serializer whose acceptance is byte-exact reproduction in a fresh emulator and which remembers the primary screen when a program leaves it, and the shell-integration observer that turns OSC 133, OSC 7, title and alternate-screen sequences into events without touching the bytes.
- **0008 — History and Pane.** The bounded per-pane history ring indexed by absolute sequence with a global budget, and the pane that assembles process, streams, mirror, history and observer behind one interface.
- **0009 — Fidelity.** Byte identity and screen reproduction over the corpus plan 0001 committed, proven in-process and inside the host container through the regression driver's new `pane` step, and a flood that proves the bounds hold.
- **0010 — Documentation.** The server crate's documentation and the root architecture brought into line with what landed, including what was learned about the emulator.

## Out of scope

No session, tab or layout exists yet: a pane is created by a test and addressed by a `PaneId` a test minted. That is plan 0003, as are the multiplexer, credit, resume and compression. No daemon runs and no socket is listened on — plan 0004. Nothing here crosses a network — plan 0005. The regression scenarios in this plan run entirely inside the host container, where the driver links the server library directly.
