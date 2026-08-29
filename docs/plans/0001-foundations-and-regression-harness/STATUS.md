# Plan 0001 — Foundations and Regression Harness — ✅ Done

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** ✅ Done.

- **Goal:** stand up the workspace under its complete rule set, the frame codec, `iznik/1` control messages and framed link pinned by goldens, the VT oracle, pseudoterminal harness and fidelity corpus, and the two-container Podman fixture with scenarios that are ordinary tests and a claims registry — every wait bounded by a deadline and the harness itself held to a measured ceiling.
- **Root cause:** every later plan makes claims about a distributed system under a real network, a real SSH connection and a real shell, which no unit test and no developer's already-configured machine can honestly check; rules that are not gates decay under pressure; time that is not bounded is lost; and a proof surface that is slow is a proof surface nobody runs.
- **Approach:** land the rules as failing builds before the first line of product code, land the proof surface before the first claim, make every process, test, scenario step and fixture wait carry a deadline that turns a hang into a named failure, and make the harness's own speed a claim with a proof.
- **Progress:** 14/14 tasks done; 0 blocked; 0 dropped.
- **Integration:** `in-progress`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** `iznik-protocol` holds a private `wire` module the module skeleton in ARCHITECTURE.md predates, hoisted out of `message.rs` after `control-messages` landed, because that file was at the length limit and plan 0003's payload codecs need the same primitives; and `frame-codec`'s decoder gained `ready` and `pending` after the task was done, because `framed-link` cannot be written against the decoder's specified surface and its touches are `iznik-link` only, then `spare` and `commit` so a read lands in the decoder's own buffer, with `FrameHeader::for_payload` owning the size rule the link had repeated. All are follow-up commits, reviewed; the plans' text is unchanged. The VT oracle departs from its section's letter in three recorded ways: every accessor returns a `Result`, because the engine's calls are fallible; cells are read in the engine's active area rather than the screen space the section names, which counts scrollback and would address the wrong row once anything has scrolled; and a `size` accessor exists because three others need the size after a resize. The testkit's manifest names `serde_json` (the golden loader returns JSON values) and `nix` (a dropped pseudoterminal child is killed by process group; the metrics ask the system for its page size and clock tick), and the link crate's tests name tokio's `time` feature, none of which the architecture's per-crate lists carry; each is an exact pin already in the resolved set. Two departures land with regression-fixture: a `.config/nextest.toml` override gives the fixture reaper test the machine to itself, because its `reap(Everything)` removes every labelled container and a global reap has no per-test fix; and `xtask/src/regression.rs`, touched by both regression-images and regression-fixture, is attributed to the latter's vocabulary, since its final content is the stage and reap forms. A host-account unlock in the host image, found by this task and fixed in its own commit, is a follow-up to regression-images.
  Plan 0004's `darwin-artifacts` left `claims-registry` one fix, recorded here because the file is this plan's. A claim's `platform` was compared against `std::env::consts::OS` outright, so the `platform = "darwin"` that plan 0004's task text asks for would have deferred on a Mac as well — a proof that never runs anywhere, reported as though it were merely waiting. The registry now knows `darwin` and `macos` for one machine, and `xtask/tests/claims.rs` holds it to that.

- **Outcome:** A rule-gated Rust workspace with golden-pinned wire primitives, a headless VT oracle, and a two-container Podman regression suite whose scenarios are parallel nextest tests and whose claims registry gates every later plan, with `cargo xtask check` as the one command that says whether a change may land.

- **Found by the soak, 2026-08-29.** A container's first process was the idle
  program — a `sleep`, which never waits — so every process orphaned into a
  container stayed in its table for ever. Nothing shows for minutes. Over
  hours the entries accumulate: a six-hour soak reached four before the engine
  held two thousand orphaned `ssh` control masters, could no longer fork, and
  failed five hundred and eighty-one rounds in twenty minutes. Every container
  now starts with an init that reaps.

  `regression_fixture_reaps_what_is_orphaned_into_it` holds both halves of it,
  because either alone passes for the wrong reason: that the first process is
  one that waits, and that fifty orphans leave nothing behind. Each was
  checked by taking the fix away — the first refuses immediately, the second
  reports "kept 50 orphans it should have reaped", which is the production
  defect in miniature.

  This task had declared no claims at all, so nothing here had ever been in
  the registry; `regression/claims/regression-fixture.toml` now carries eleven,
  one for each of its cases.

_Last updated: 2026-08-29, against `develop` @ `e0cf2ae`._
