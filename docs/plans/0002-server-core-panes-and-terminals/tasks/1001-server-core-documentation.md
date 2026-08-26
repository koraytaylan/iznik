---
id: server-core-documentation
title: "Server Core Documentation"
workstream: "0010"
kind: task
depends_on:
  - fidelity-suite
gated: false
touches:
  - ARCHITECTURE.md
  - "crates/iznik-server/README.md"
  - "crates/iznik-testkit/README.md"
  - "crates/iznik-regression/README.md"
  - "docs/notes/terminal-mirror.md"
  - "policy/lexicon/server-core-documentation.txt"
status: planned
merged_as: ""
---
# Server Core Documentation

Two tasks in this plan confirmed how the emulator behaves rather than assuming it. This task makes sure the next person does not have to rediscover it, and that the crate READMEs describe the modules that exist.

**Steps:**

1. Write `docs/notes/terminal-mirror.md`: which queries `libghostty-vt` 0.2.1 answers through `on_pty_write` and which through the dedicated effects, what the mirror answers for each, what the formatter emits, how the alternate screen is reproduced and why, each with the test that established it.
2. Update the root `ARCHITECTURE.md` §5.1 to state the confirmed behavior, and the READMEs of `iznik-server`, `iznik-testkit` and `iznik-regression` to document every module and the `pane` step.

**Tests:**

- The documentation and links policy checks pass: every module appears in its crate's README, every relative link resolves.
- Every finding in `docs/notes/terminal-mirror.md` names a test in `crates/iznik-server/tests/` that exists.

- **Done when:** `timeout 3600 cargo xtask check` succeeds and every test path named in `docs/notes/terminal-mirror.md` exists.
