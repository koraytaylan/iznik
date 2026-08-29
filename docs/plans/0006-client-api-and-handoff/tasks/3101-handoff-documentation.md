---
id: handoff-documentation
title: "Handoff Documentation"
workstream: "0031"
kind: task
depends_on:
  - client-contract
  - soak-and-release
gated: false
touches:
  - README.md
  - ARCHITECTURE.md
  - CONTRIBUTING.md
  - "crates/*/README.md"
  - "xtask/README.md"
  - "policy/lexicon/handoff-documentation.txt"
status: done
merged_as: ""
---
# Handoff Documentation

Six plans have landed. This task makes the documents say exactly what is true — no more, no less — and tells the person building the macOS application where to start.

**Steps:**

1. Add "Building the macOS application against this repository" to `README.md`: the artifacts directory, the header, the static library, the contract, the `unix:` alias for a local daemon, and the diagnostics command.
2. Revise the root `ARCHITECTURE.md` and `CONTRIBUTING.md` end to end against the code, and every crate README against its modules.
3. Remove every statement that describes something that does not exist.

**Tests:**

- The documentation and links policy checks pass.
- Every command in `README.md` and `CONTRIBUTING.md` answers `--help` with exit 0 under a deadline.
- `git grep -n -i -E 'planned|will be|not yet' -- README.md ARCHITECTURE.md CONTRIBUTING.md 'crates/*/README.md'` prints nothing, so the documents describe the present.

- **Done when:** `timeout 3600 cargo xtask check` succeeds, every command the root documents name answers `--help` with exit 0, and `git grep -n -i -E 'planned|will be|not yet' -- README.md ARCHITECTURE.md CONTRIBUTING.md 'crates/*/README.md'` prints nothing.
