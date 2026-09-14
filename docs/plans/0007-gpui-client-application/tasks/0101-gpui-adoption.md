---
id: gpui-adoption
title: "Adopt GPUI Kit: scaffold iznik-app and prove the gate headless"
workstream: "0001"
kind: task
depends_on: []
gated: true
touches:
  - Cargo.toml
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/README.md
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/main.rs
  - crates/iznik-app/tests/headless_smoke.rs
  - policy/dependencies.md
  - policy/lexicon/gpui-adoption.txt
  - regression/claims/gpui-adoption.toml
status: planned
merged_as: ""
---
# Adopt GPUI Kit: scaffold iznik-app and prove the gate headless

Scaffold the application crate into the workspace, written once with every dependency the plan needs, and prove before anything builds on it that a GPUI crate passes this workspace's gates on the headless Linux development machine.

**Steps:**

1. Add `crates/iznik-app` as a workspace member; write its manifest once: dependencies `gpui-kit` pinned exact at its current release (0.6.1) with the icon crate the kit's documentation names, `iznik-client` and `iznik-protocol` for the engine, and nothing else the plan does not name; development dependency on `iznik-testkit` for the in-process stack. Record the resolved `gpui` version the pin produces.
2. Write `policy/dependencies.md` entries with one-sentence justifications for every crate the pin adds, so the dependency allowlist equals the resolved set in both directions.
3. Add `policy/lexicon/gpui-adoption.txt` with every new word the crate's names introduce.
4. Write the crate root per house rules, its documentation included from the crate's own README, and a binary `iznik-app` that opens one GPUI window with a themed empty view, answers `--help`, and refuses no display at link time — display absence is a runtime condition, not a build failure.
5. Write `crates/iznik-app/tests/headless_smoke.rs` using GPUI's test context to build a window with one styled element and assert on the resulting element tree, proving GPUI tests run headlessly under nextest.
6. Declare this task's claims in `regression/claims/gpui-adoption.toml` as `test` proofs with their `because`.

**Tests:**

- The headless smoke test builds an element tree and asserts its shape without a display.
- The dependency allowlist resolves to exactly the declared set, and no dependency is unjustified.
- `cargo doc` is warning-free for the new crate, private items included.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task gpui-adoption` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
