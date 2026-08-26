# Plan 0001 — Foundations and Regression Harness — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** stand up the workspace under its complete rule set, the frame codec, `iznik/1` control messages and framed link pinned by goldens, the VT oracle, pseudoterminal harness and fidelity corpus, and the two-container Podman fixture with scenarios that are ordinary tests and a claims registry — every wait bounded by a deadline and the harness itself held to a measured ceiling.
- **Root cause:** every later plan makes claims about a distributed system under a real network, a real SSH connection and a real shell, which no unit test and no developer's already-configured machine can honestly check; rules that are not gates decay under pressure; time that is not bounded is lost; and a proof surface that is slow is a proof surface nobody runs.
- **Approach:** land the rules as failing builds before the first line of product code, land the proof surface before the first claim, make every process, test, scenario step and fixture wait carry a deadline that turns a hang into a named failure, and make the harness's own speed a claim with a proof.
- **Progress:** 6/14 tasks done; 0 blocked; 0 dropped.
- **Integration:** `in-progress`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** `iznik-protocol` holds a private `wire` module the module skeleton in ARCHITECTURE.md predates, hoisted out of `message.rs` after `control-messages` landed, because that file was at the length limit and plan 0003's payload codecs need the same primitives; and `frame-codec`'s decoder gained `ready` and `pending` after the task was done, because `framed-link` cannot be written against the decoder's specified surface and its touches are `iznik-link` only. Both are follow-up commits, reviewed; the plans' text is unchanged.
- **Outcome:** A rule-gated Rust workspace with golden-pinned wire primitives, a headless VT oracle, and a two-container Podman regression suite whose scenarios are parallel nextest tests and whose claims registry gates every later plan, with `cargo xtask check` as the one command that says whether a change may land.

_Last updated: 2026-08-26, against `develop`._
