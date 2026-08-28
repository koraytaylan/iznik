---
id: client-documentation
title: "Client Documentation"
workstream: "0025"
kind: task
depends_on:
  - end-to-end-ssh
gated: false
touches:
  - README.md
  - ARCHITECTURE.md
  - "crates/iznik-client/README.md"
  - "crates/iznik-regression/README.md"
  - "policy/lexicon/client-documentation.txt"
status: done
merged_as: ""
---
# Client Documentation

A tool that installs binaries on other people's machines says so up front. This task writes what iznik puts on a host and how to take it off, and brings the client's documentation into line with what landed.

**Steps:**

1. Add "What iznik puts on a host" to `README.md`: the prefix and how it is chosen, the binary, the terminfo, the runtime directory, the log, the daemon's lifetime, and `uninstall`.
2. Update the root `ARCHITECTURE.md` §6 to what landed, including the `unix:` alias, and the `iznik-client` and `iznik-regression` READMEs to document every module and step.

**Tests:**

- The documentation and links policy checks pass.
- Every path named in "What iznik puts on a host" is a constant in `crates/iznik-client/src/bootstrap/`, asserted by a test that greps both.

- **Done when:** `timeout 3600 cargo xtask check` succeeds and `timeout 600 cargo nextest run --package iznik-client -E 'test(documented_paths)'` proves every documented path matches a bootstrap constant.
