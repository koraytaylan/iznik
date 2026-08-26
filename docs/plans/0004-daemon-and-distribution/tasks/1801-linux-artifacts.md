---
id: linux-artifacts
title: "Linux Artifacts"
workstream: "0018"
kind: task
depends_on:
  - stdio-relay
gated: false
touches:
  - "xtask/src/distribution/mod.rs"
  - "xtask/src/distribution/linux.rs"
  - "xtask/tests/regression_distribution_linux.rs"
  - "regression/claims/linux-artifacts.toml"
  - "policy/lexicon/linux-artifacts.txt"
status: planned
merged_as: ""
---
# Linux Artifacts

The bootstrap uploads one file to a host whose libc version it does not know. That file is a stripped, statically linked musl binary, reproducible from the same commit, with its digest in a manifest a person or a pipeline can verify; the bootstrap verifies digests it computes itself.

**Steps:**

1. Implement `xtask::distribution` — `mod.rs` with the manifest and checksum writing shared by every target, `linux.rs` for the two musl triples under the `release` profile with `--remap-path-prefix` — and fill the `xtask distribution --target <triple>` subcommand.
2. Write `xtask/tests/regression_distribution_linux.rs`, `#[ignore]`, including the case that assembles a staging directory around the x86_64 artifact, starts the fixture on it, and runs `--version` inside `host0`.
3. Declare this task's claims in `regression/claims/linux-artifacts.toml` as `test` proofs, the container case with the `because` that the artifact under test is built by the test.

**Tests:**

- Both triples build, producing a stripped static executable each, asserted by ELF header inspection: no `PT_INTERP`, the expected `e_machine`.
- Reproducible: two consecutive builds for one triple produce byte-identical binaries and identical `SHA256SUMS`.
- The manifest records the crate version, the protocol version, the triple and a digest that matches the file.
- Size: each binary is under `ARTIFACT_SIZE_CEILING`.
- In the container: the x86_64 artifact prints its version inside `host0`.

- **Done when:** `timeout 1800 cargo nextest run --package xtask --test regression_distribution_linux --run-ignored all` passes every case above, `timeout 1800 cargo xtask claims verify --task linux-artifacts` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
