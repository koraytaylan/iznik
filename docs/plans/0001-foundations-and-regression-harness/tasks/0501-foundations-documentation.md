---
id: foundations-documentation
title: "Foundations Documentation"
workstream: "0005"
kind: task
depends_on:
  - claims-registry
gated: false
touches:
  - README.md
  - ARCHITECTURE.md
  - CONTRIBUTING.md
  - "crates/*/README.md"
  - "xtask/README.md"
  - "policy/lexicon/foundations-documentation.txt"
status: planned
merged_as: ""
---
# Foundations Documentation

The design documents were written before the code. This task makes them true: every command they name exists, every module they list is the module that landed, every crate README documents every module the crate has, and every number the harness is held to is the number that was measured.

**Steps:**

1. Update the root `README.md` — status, layout and commands — and the root `ARCHITECTURE.md` §3 crate table and §8 to reflect exactly what plan 0001 landed, and `CONTRIBUTING.md` §1, §2 and §5 to name the real prerequisites, gates, deadlines and test tiers, with the measured fixture start time and a typical claims-gate duration stated.
2. Rewrite each crate `README.md` as the crate's documentation: purpose, a table of every module with one sentence each, and how the crate is tested.
3. Remove every statement that names a crate, module, command or file that does not exist in the tree.

**Tests:**

- The documentation and links policy checks pass: every module appears in its crate's README, every relative link resolves, every fence is tagged.
- Every backticked `crates/…`, `xtask/…`, `regression/…` and `docs/…` path in the root documents and crate READMEs exists, asserted by a shell loop in the done-when.
- Every command in `README.md` and `CONTRIBUTING.md` §2 is a subcommand the dispatchers route, asserted by running each with `--help` under a deadline.

- **Done when:** `timeout 3600 cargo xtask check` succeeds, every backticked repository path in `README.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md` and the crate READMEs exists on disk, and `timeout 60 cargo xtask --help` lists every subcommand `README.md` names.
