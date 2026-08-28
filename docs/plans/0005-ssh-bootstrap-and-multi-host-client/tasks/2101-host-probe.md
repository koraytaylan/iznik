---
id: host-probe
title: "Host Probe"
workstream: "0021"
kind: task
depends_on:
  - ssh-control-master
gated: false
touches:
  - "crates/iznik-client/src/bootstrap/probe.rs"
  - "crates/iznik-client/tests/host_probe.rs"
  - "crates/iznik-regression/src/step/probe.rs"
  - "regression/claims/host-probe.toml"
  - "regression/scenarios/host-probe/**"
  - "policy/lexicon/host-probe.txt"
status: done
merged_as: ""
---
# Host Probe

Everything the bootstrap needs to know about a host, in one round trip, because latency to a distant host is the dominant cost — including the cases that decide whether the bootstrap feels good: an unwritable home, an unexpected architecture, a host without `tic`.

**Steps:**

1. Implement `crates/iznik-client/src/bootstrap/probe.rs` — the remote script, `probe`, the pure `parse`, `HostProbe`, `OperatingSystem`, `Architecture`, `InstalledServer`, the prefix fallback, `ProbeError` — exactly as the architecture's `host-probe` section specifies.
2. Implement `crates/iznik-regression/src/step/probe.rs` — `[steps.probe]` with `alias` and its `expect` table.
3. Write `crates/iznik-client/tests/host_probe.rs` for `parse`, and the scenarios under `regression/scenarios/host-probe/` — `fresh-host`, `unwritable-home` — from the engine.
4. Declare this task's claims in `regression/claims/host-probe.toml`; the `tic`-absent case is a `test` proof, because the absence of a binary is a string in the probe's output and the container cannot be made to lack one it has.

**Tests:**

- `parse` on synthetic outputs: Linux x86_64 and aarch64, Darwin arm64; an installed server with its versions; no server; terminfo present and absent; `tic` present and absent; each prefix candidate writable and not; an unknown operating system yields `Unsupported`.
- Prefix fallback: with `$XDG_DATA_HOME` unwritable the prefix is `~/.local/share/iznik`; with that unwritable too, the runtime directory; the chosen one is reported.
- One round trip: `probe` issues exactly one remote command, asserted by counting spawns through a transport wrapper.
- In the container: a fresh `host0` probes as Linux x86_64 with no server, `tic` available, terminfo absent, and the default prefix; with the home directory made unwritable by a preceding step, the prefix falls back and the probe still succeeds; the home directory is restored by a following step.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test host_probe` passes every pure case, `timeout 900 cargo xtask claims verify --task host-probe` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
