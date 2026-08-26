---
id: claims-registry
title: "Claims Registry"
workstream: "0004"
kind: task
depends_on:
  - policy-gates
  - gate-runner
  - control-messages
  - framed-link
  - vt-oracle
  - pty-harness
  - fidelity-corpus
  - scenario-driver
gated: false
touches:
  - "xtask/src/claims/**"
  - "xtask/tests/claims.rs"
  - "xtask/tests/fixtures/claims/**"
  - "regression/claims/claims-registry.toml"
  - "regression/scenarios/claims-registry/**"
  - "docs/notes/claims.md"
  - "policy/lexicon/claims-registry.txt"
status: done
merged_as: ""
---
# Claims Registry

The mechanism that turns "every implementation comes with acceptance criteria" from a habit into a build failure. Every task from here on declares what it claims about runtime behavior as data and names the test that proves it; a claim without a passing proof, a proof for a claim that does not exist, and a change to product code that declares no claims each fail the gate. This task lands last in the plan so that it is not a precondition for the tasks that build it, and it is not exempt from itself.

**Steps:**

1. Write `docs/notes/claims.md` first — the format, the rules, the selection logic, the nextest filter that runs one scenario by hand, and the reasoning — as the worked example every later task copies, then `regression/claims/claims-registry.toml` declaring this task's own claims and the scenarios under `regression/scenarios/claims-registry/` that prove them.
2. Implement `xtask::claims` — `registry::load` with every validation the architecture's `claims-registry` section lists, `selection::select` with `Selection::{Tasks, Everything, CurrentBranch}` and the product-code rule, `verify::verify` building the filterset, running nextest under the `claims` profile and reading its JUnit report — and fill the `xtask claims verify [--task <id>]… [--root <dir>]` and `xtask claims coverage [--root <dir>]` subcommands.
3. Write `xtask/tests/claims.rs` with synthetic registries and captured JUnit reports under `xtask/tests/fixtures/claims/`.

**Tests:**

- Validation, each on its own synthetic root: a duplicate claim id names both files; a `test` proof without `because`, a claim with both proofs, and a claim with neither are each rejected; a claims file named after no task is rejected; a scenario file naming an undeclared claim is rejected; a `scenario` proof naming a file that does not exist is rejected.
- Selection: `--task` is explicit; on a branch whose diff since the merge base changes two claims files, both tasks are selected; a diff that changes a file under `crates/iznik-server/src/` and no claims file fails naming the rule; a diff that changes only tests, fixtures or documentation selects nothing and passes saying so; with no `regression/claims/` directory the rule is inactive.
- Verify, against captured JUnit reports: a claim whose test passed is proven; one whose test failed is failed with the test's name; one whose test does not appear in the report is missing, never proven; one with a `platform` this machine cannot satisfy is deferred and counted separately; one with `profile = "regression"` is run in its own invocation, asserted by the recorded command lines.
- The filterset: a `scenario` proof becomes `test(=scenario::<task>::<name>)`, a `test` proof becomes `package(…) & binary(…) & test(=…)`, the union is what a single invocation receives, and the invocation names exactly the packages the proofs name with `--package`.
- Self-application: this task's own claims — "a scenario proof is proven by its scenario passing" and "a scenario proof runs inside the container it names" — are proven by its own scenarios under the same `coverage` run as everything else; that a failing scenario fails its claim is proven by the captured-report case above, never by a scenario committed to fail.
- The real tree: `xtask claims coverage` exits 0.

- **Done when:** `timeout 600 cargo nextest run --package xtask --test claims` passes every synthetic case, `timeout 900 cargo xtask claims verify --task claims-registry` reports every claim this task declares as proven, `timeout 900 cargo xtask claims coverage` exits 0 on the real tree, and `timeout 3600 cargo xtask check` succeeds.
