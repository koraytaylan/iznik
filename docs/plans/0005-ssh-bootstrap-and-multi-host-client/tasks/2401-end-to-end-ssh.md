---
id: end-to-end-ssh
title: "End to End over SSH"
workstream: "0024"
kind: task
depends_on:
  - connection-manager
gated: false
touches:
  - "crates/iznik-regression/src/step/manager.rs"
  - "regression/claims/end-to-end-ssh.toml"
  - "regression/scenarios/end-to-end-ssh/**"
  - "policy/lexicon/end-to-end-ssh.txt"
status: done
merged_as: ""
---
# End to End over SSH

The whole stack, from a machine that had nothing, against two hosts, through real link drops: the product's first end-to-end run and the plan's proof. Every scenario finishes in under two minutes, because every interval the manager waits on is a field the scenario sets.

**Steps:**

1. Implement `crates/iznik-regression/src/step/manager.rs` — `[steps.manager]` with `artifacts`, every `ManagerOptions` timing as a field, and its actions — exactly as the architecture's `end-to-end-ssh` section specifies, running a `HostManager` inside the engine container.
2. Author the scenarios under `regression/scenarios/end-to-end-ssh/` with `hosts = 2` — `two-hosts-from-nothing`, `one-host-drops`, `pane-identity-survives`, `upgrade-policy`, `uninstall-everything` — with pong deadlines and backoff in hundreds of milliseconds.
3. Declare this task's claims in `regression/claims/end-to-end-ssh.toml`, each proven by one of those scenarios.

**Tests:**

- From nothing: both hosts are bootstrapped from the bare engine, a session with a typed line exists on each, and the screens through the oracle show the lines.
- One host drops: with a network fault on `host1`, `host0`'s keystroke echo stays under the latency budget; when the fault clears, `host1` reconnects with backoff within seconds.
- Identity and bytes survive: after the reconnection, `host1`'s pane has the same `GlobalPaneId` and its bytes continue byte-exact from the held cursor, reassembled inside the engine against an unbroken stream with `expect_reassembly`.
- Upgrade policy: with the older-version shim installed and a live pane, the upgrade is refused with the count; forced, it succeeds and the real version answers.
- Uninstall: both hosts are left with no prefix, no runtime directory and no process.

- **Done when:** `timeout 900 cargo xtask claims verify --task end-to-end-ssh` reports every claim proven and `timeout 3600 cargo xtask check` succeeds.
