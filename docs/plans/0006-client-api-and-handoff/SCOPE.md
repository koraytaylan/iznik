# Scope — Plan 0006

> Publish the contract the macOS application is built against, prove the ABI with a C program rather than a promise, and hand over everything needed to diagnose the stack from the outside — because the application is built by someone else, somewhere else.

## Why this plan

Every plan before this one built a complete remote terminal system with no way to see it. This plan puts a boundary on it: a C ABI over `iznik-client`, a byte-pipe surface shaped for feeding a libghostty surface directly, and a written contract precise enough that a Swift developer can build against it without reading Rust and without asking.

That last part carries more weight than it usually would. The macOS application lives in another repository, on a machine this one never runs on, in a language none of this code is written in. The contract is therefore not documentation of the implementation — it is the specification the implementation is held to, and where the two disagree the contract wins. It has to answer the questions that come up at two in the morning: which thread does this callback arrive on, what happens to a surface when its host drops, who decides a pane's size, what a `Screen` obliges the application to do, whose emulator answers a program's query, and what the application must never do.

The other half is diagnosis. When something is wrong in a system spanning a native application, a Rust engine, an SSH link, a remote daemon and a shell, the person debugging it needs one command that says which of those is broken. That command is worth more than any feature. And a system like this fails on the timescale of days, not tests: a leak of a few kilobytes per reconnect is invisible in CI and fatal by Thursday, so the soak runs for hours before the release checklist is allowed to say yes.

## In scope

- **0027 — FFI Surface.** The C ABI over `iznik-client`: lifecycle, hosts, an event callback carrying the protocol's own encoding, command submission, errors with a layer tag; and the per-pane byte pipe with mandatory credit, input, resize and focus.
- **0028 — Build and ABI Stability.** Header generation, the static library, a golden header test so an accidental ABI change is a failing test here rather than a crash in someone else's application, and a C smoke program that exercises the ABI end to end against a local daemon.
- **0029 — Diagnosis.** The `iznik` plumbing commands, and `iznik doctor <host>` collecting every layer's state into one redacted artifact a person can read or send.
- **0030 — Handoff.** `docs/CLIENT.md`, the normative contract, and a soak plus release checklist that proves the system holds over time rather than over a test run.
- **0031 — Documentation.** The final pass over the root and crate documentation, including how the application integrates.

## Out of scope

No macOS, Swift or Objective-C code is written in this repository, and no Xcode project lives here. Packaging, signing, notarization and distribution of the application belong to whoever ships it. Predictive local echo remains excluded; the contract reserves the shape for it. Scrollback search and any query API over history stay out until a client asks for them. A Darwin static library is built by the application's own build or by the gated Darwin workflow, not on the Linux development machine.
