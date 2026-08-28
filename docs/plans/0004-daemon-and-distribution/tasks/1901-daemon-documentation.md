---
id: daemon-documentation
title: "Daemon Documentation"
workstream: "0019"
kind: task
depends_on:
  - performance-baseline
  - linux-artifacts
gated: false
touches:
  - README.md
  - ARCHITECTURE.md
  - "crates/iznik-server/README.md"
  - "crates/iznik-testkit/README.md"
  - "crates/iznik-regression/README.md"
  - "xtask/README.md"
  - "policy/lexicon/daemon-documentation.txt"
status: done
merged_as: ""
---
# Daemon Documentation

The daemon exists, has numbers, and ships. This task makes the documents say so: how it is started and stopped, where it lives on a host, what it costs, and how an artifact is built and verified.

**Steps:**

1. Update the root `ARCHITECTURE.md` §5.5 and §8 to what landed, and `README.md` with the distribution commands and a link to the baseline.
2. Update the READMEs of `iznik-server`, `iznik-testkit`, `iznik-regression` and `xtask` to document every module and every scenario step that exists.

**Tests:**

- The documentation and links policy checks pass.
- Every command named in `README.md` exists, asserted by running each with `--help` under a deadline.

- **Done when:** `timeout 3600 cargo xtask check` succeeds and every `xtask` and `iznik-server` command `README.md` names answers `--help` with exit 0.
