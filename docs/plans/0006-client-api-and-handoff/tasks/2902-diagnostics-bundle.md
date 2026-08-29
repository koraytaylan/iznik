---
id: diagnostics-bundle
title: "Diagnostics Bundle"
workstream: "0029"
kind: task
depends_on:
  - plumbing-commands
gated: false
touches:
  - "crates/iznik-cli/src/doctor.rs"
  - "crates/iznik-cli/tests/doctor.rs"
  - "regression/claims/diagnostics-bundle.toml"
  - "regression/scenarios/diagnostics-bundle/**"
  - "policy/lexicon/diagnostics-bundle.txt"
status: done
merged_as: ""
---
# Diagnostics Bundle

When something is wrong in a system spanning a native application, a Rust engine, an SSH link, a remote daemon and a shell, one command says which layer is broken — and never leaks a secret while doing it.

**Steps:**

1. Implement `crates/iznik-cli/src/doctor.rs` — every section the architecture's `diagnostics-bundle` section lists, the `ssh -G` capture, and redaction by construction — filling the `iznik doctor <host>` stub.
2. Write `crates/iznik-cli/tests/doctor.rs` against an in-process `Stack`, and the scenarios under `regression/scenarios/diagnostics-bundle/` — `healthy-host`, `unreachable-host` (with a two-second connect timeout), `server-not-installed` — from the engine.
3. Declare this task's claims in `regression/claims/diagnostics-bundle.toml`.

**Tests:**

- The bundle is one JSON document with every section present, each either filled or carrying the error that prevented it, so a failing layer is visible by its absence of data and its presence of an error.
- Redaction: with an SSH configuration naming an identity file, an agent socket and a passphrase-protected key, and an environment carrying tokens, the bundle contains none of the key material, no token values and no environment values — asserted by scanning the output for planted sentinel strings.
- The `ssh -G` section is captured from the command's output and never parsed from `~/.ssh/config`; for a `unix:` host the section says so and is skipped.
- In the container: a healthy host yields a bundle whose round-trip figure is under the latency budget; an unreachable alias yields a bundle whose transport section carries `Unreachable` within the timeout and whose later sections carry "skipped"; a host without the server yields a probe section saying so.

- **Done when:** `timeout 600 cargo nextest run --package iznik-cli --test doctor` passes every local case, `timeout 900 cargo xtask claims verify --task diagnostics-bundle` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
