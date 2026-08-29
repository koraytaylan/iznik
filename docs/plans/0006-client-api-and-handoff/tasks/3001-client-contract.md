---
id: client-contract
title: "Client Contract"
workstream: "0030"
kind: task
depends_on:
  - static-library-and-header
gated: false
touches:
  - "docs/CLIENT.md"
  - "crates/iznik-ffi/README.md"
  - "xtask/tests/contract_matches_header.rs"
  - "regression/claims/client-contract.toml"
  - "policy/lexicon/client-contract.txt"
status: done
merged_as: ""
---
# Client Contract

The specification the implementation is held to, written for a Swift developer who cannot read this repository and cannot ask it questions. Where the contract and the implementation disagree, the contract wins and the implementation is the bug.

**Steps:**

1. Write `docs/CLIENT.md` covering every item the architecture's `client-contract` section lists, publishing the obligations the header declares verbatim, with a worked example for attaching a pane to a surface, returning credit, handling a screen, forwarding query responses, and handling the alternate-screen marks.
2. Rewrite `crates/iznik-ffi/README.md` to point at the contract and document every module.
3. Write `xtask/tests/contract_matches_header.rs`, and declare this task's claims in `regression/claims/client-contract.toml` as `test` proofs with their `because`.

**Tests:**

- Every function, type and constant named in `docs/CLIENT.md` exists in `include/iznik.h`, and every one in the header is named in the contract.
- Every obligation stated in the header's `Obligation:` lines appears verbatim in the contract.
- The documentation and links policy checks pass.

- **Done when:** `timeout 600 cargo nextest run --package xtask --test contract_matches_header` passes every case above, `timeout 900 cargo xtask claims verify --task client-contract` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
