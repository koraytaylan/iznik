---
id: darwin-artifacts
title: "Darwin Artifacts"
workstream: "0018"
kind: task
depends_on:
  - linux-artifacts
gated: true
touches:
  - "xtask/src/distribution/darwin.rs"
  - "xtask/tests/regression_distribution_darwin.rs"
  - ".github/workflows/darwin-artifacts.yml"
  - "regression/claims/darwin-artifacts.toml"
  - "policy/lexicon/darwin-artifacts.txt"
status: planned
merged_as: ""
---
# Darwin Artifacts

Remote hosts are not always Linux — a Mac under a desk is a common target. This task extends distribution to Darwin. It is gated because cross-compiling to Darwin requires an SDK that cannot be assumed on a Linux development machine; a human decides how that toolchain is provided before the task runs, and the workflow builds the artifacts where the toolchain is native.

**Steps:**

1. Document the required toolchain in the module header — SDK provisioning, linker, environment variables — so the gate has a concrete thing for a person to satisfy.
2. Implement `xtask::distribution::darwin` for `aarch64-apple-darwin` and `x86_64-apple-darwin`, reusing the shared manifest and checksum path, and detect a missing toolchain with a message naming exactly what is absent.
3. Write `.github/workflows/darwin-artifacts.yml` building both targets on a macOS runner and uploading the artifacts and manifest, and `xtask/tests/regression_distribution_darwin.rs`, `#[ignore]`.
4. Declare this task's claims in `regression/claims/darwin-artifacts.toml` with `platform = "darwin"` where the proof needs a Mac, so they report as deferred rather than proven here.

**Tests:**

- Both Darwin triples build where the toolchain is present, producing Mach-O executables whose CPU type matches the triple, asserted by header inspection.
- Dynamic-link surface: each binary links only system libraries present on a stock macOS install, asserted against a stated allowlist.
- The manifest and checksums come from the shared code path.
- Missing-toolchain diagnostics: with the SDK environment unset, the build fails with the documented message and produces no partial artifact.
- Size: each binary is under `ARTIFACT_SIZE_CEILING`.

- **Done when:** `timeout 1800 cargo nextest run --package xtask --test regression_distribution_darwin --run-ignored all` passes every case that can run on this machine and reports the rest as deferred, `timeout 900 cargo xtask claims verify --task darwin-artifacts` reports every claim proven or deferred and none unproven, and `timeout 3600 cargo xtask check` succeeds.
