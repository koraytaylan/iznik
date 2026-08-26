---
id: fidelity-suite
title: "Fidelity Suite"
workstream: "0009"
kind: task
depends_on:
  - pane-assembly
gated: false
touches:
  - "crates/iznik-server/tests/fidelity.rs"
  - "crates/iznik-regression/src/step/pane.rs"
  - "regression/claims/fidelity-suite.toml"
  - "regression/scenarios/fidelity-suite/**"
  - "policy/lexicon/fidelity-suite.txt"
status: planned
merged_as: ""
---
# Fidelity Suite

The argument for the architecture: what a program writes is what a client receives, byte for byte, for every construct that breaks naive implementations, at volume, through the statically linked binary on the image a real host resembles. This is also where the regression driver learns to drive a pane, which every later plan's scenarios build on.

**Steps:**

1. Implement `crates/iznik-regression/src/step/pane.rs` — the `[steps.pane]` table with `program`, `send` in its three forms, `capture_to`, `screen_to` and `expect_screen_reproduction` — exactly as the architecture's `fidelity-suite` section specifies, driving a `Pane` on a `MirrorThread` inside the container.
2. Write `crates/iznik-server/tests/fidelity.rs` for the in-process proofs, and the scenarios under `regression/scenarios/fidelity-suite/` — `byte-identity`, `screen-reproduction`, `flood` — for the container proofs, each with the corpus golden in its `files`.
3. Declare this task's claims in `regression/claims/fidelity-suite.toml`, each proven by one of those scenarios.

**Tests:**

- Byte identity, per construct: for each construct, the bytes read from the pane's history equal what the child wrote, with a failure reporting the construct's name and the first differing offset.
- Screen reproduction: for each construct, the serialized screen fed into a `Vt` equals a `Vt` fed the construct directly.
- Flood: 64 MiB of generated text written by `cat` arrives byte-identical from history where the ring still holds it, `newest` equals the total length, the ring holds exactly its capacity, the process's resident memory stays under a stated ceiling throughout, and the whole case runs in under five seconds.
- In the container: `byte-identity` and `screen-reproduction` run the same assertions through `[steps.pane]` inside `host0` against the musl `iznik-regression` with `expect_screen_reproduction`, and `flood` asserts the memory ceiling there through a `ps` census step.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test fidelity` passes every in-process case, `timeout 900 cargo xtask claims verify --task fidelity-suite` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
