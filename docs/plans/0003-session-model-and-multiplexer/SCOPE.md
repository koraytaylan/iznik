# Scope — Plan 0003

> Bytes are not a session. Give the server an authoritative model of sessions, tabs and panes, the commands that change it, the deltas that announce the change, and the multiplexer that carries every pane a client watches over one link without letting a flood starve a keystroke.

## Why this plan

Plan 0002 built a pane. A client needs to know which panes exist, how a tab arranges them, what each is called and where its shell is, and it needs that to stay true across a reconnect and across a second client on another machine. That is the host model: identity minted once and never reused, position always a derived field, and every change a numbered delta a client applies or, having missed one, replaces with a snapshot. The property the whole client design rests on — a snapshot followed by deltas converges on the next snapshot — is stated here and proven by generating random operation sequences, not by example.

The second half of this plan is the link. One connection carries every pane a client subscribes to, so without arbitration a `cat` of a large file starves the pane the user is typing into. The multiplexer is therefore a scheduler with per-channel credit and a focused pane that goes first, and its acceptance is a measured keystroke round trip under a flood — a functional test that both panes "work" would pass on an implementation nobody could stand to use. Because plan 0002 made the history ring the queue, the multiplexer never buffers pane bytes of its own: a subscription is a cursor, credit decides how far it advances, and a background pane that falls too far behind is caught up with a `Screen` rather than with a backlog.

Resume is where the two halves meet: a reconnecting client names the byte it holds, and the server either continues from it exactly or sends the truth at a named sequence. There is one mechanism, and it is tested from both sides of the boundary.

## In scope

- **0011 — Session Model.** The host model types and their invariants with a validator, the layout tree with its normalization rule, the deltas and the reconciler with the convergence property, and the session-command vocabulary with its outcomes — all golden-pinned in `iznik-protocol`.
- **0012 — Session Registry.** The server's authoritative model: operations that mint identity, spawn and close panes, arrange layouts and cascade removals; ingestion of pane events into title, directory, size and exit deltas; and the command applier that validates, applies and answers exactly once.
- **0013 — Multiplexer.** The channel table with its acknowledgement rule and the credit windows as pure tables, then the multiplexer that assembles them: fan-out of deltas and marks, the credit-based scheduler with focus priority, round-robin fairness, stale marking, the resume paths proven through the oracle, and a measured latency budget.
- **0014 — Resume and Compression.** The pure decision of what a subscription starts with, and streaming zstd in `iznik-link` with a dictionary trained on the corpus golden, kept only if its measured numbers earn it.
- **0015 — Documentation.** The protocol reference the macOS repository will read, and the crate documentation brought into line.

## Out of scope

No process listens on a socket and no client connects: the multiplexer is driven through its Rust interface and an in-memory frame sink here, and the daemon, the per-client connection loop and the relay are plan 0004. Nothing crosses a network — plan 0005. Floating panes, stacked panes, zoom state and pane-level names are not in the model; they are added when a client needs them shared.
