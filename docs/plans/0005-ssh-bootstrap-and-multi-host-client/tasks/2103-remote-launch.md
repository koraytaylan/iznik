---
id: remote-launch
title: "Remote Launch"
workstream: "0021"
kind: task
depends_on:
  - payload-upload
  - remote-channel
gated: false
touches:
  - "crates/iznik-client/src/bootstrap/mod.rs"
  - "crates/iznik-client/src/bootstrap/launch.rs"
  - "crates/iznik-client/tests/remote_launch.rs"
  - "crates/iznik-regression/src/step/bootstrap.rs"
  - "regression/claims/remote-launch.toml"
  - "regression/scenarios/remote-launch/**"
  - "policy/lexicon/remote-launch.txt"
status: planned
merged_as: ""
---
# Remote Launch

Probe, upload if needed, launch through the relay, handshake — and never again on the next connection. Because the daemon is the sessions, an upgrade is explicit and a daemon holding panes refuses it with the count; and because a tool that installs binaries on other people's machines owes them a way to take them off, `uninstall` leaves nothing behind.

**Steps:**

1. Implement `crates/iznik-client/src/bootstrap/mod.rs` and `launch.rs` — `bootstrap`, `BootstrapOptions`, `Decision`, `Bootstrapped`, `upgrade`, `uninstall`, `BootstrapError` with its stages, `UpgradeError`, `FAST_RECONNECT_BUDGET` — exactly as the architecture's `remote-launch` section specifies.
2. Implement `crates/iznik-regression/src/step/bootstrap.rs` — `[steps.bootstrap]` with `alias`, `action`, `force` and its `expect`.
3. Write `crates/iznik-client/tests/remote_launch.rs` for the decision logic against synthetic probes, and the scenarios under `regression/scenarios/remote-launch/` — `first-connection`, `second-connection-is-fast`, `upgrade-refused-for-live-panes` and `upgrade-forced` (both installing the older-version shim the architecture describes with a `run` step first), `uninstall` — from the engine.
4. Declare this task's claims in `regression/claims/remote-launch.toml`.

**Tests:**

- Decisions: no server yields `Install`; a matching version yields `UpToDate`; an older installed version yields `UpgradeAvailable`; an unsupported host yields `Unsupported` before anything is uploaded; a scripted server answering `Error { ProtocolVersion }` yields a `BootstrapError` at stage `Handshake` with the server's version.
- In the container: the first bootstrap of a fresh `host0` installs, launches and handshakes, and the daemon is running afterwards; the second bootstrap performs no upload and completes under `FAST_RECONNECT_BUDGET`; with the shim installed and a pane created, `upgrade` without `force` fails with `LivePanes { count: 1 }` and the old daemon keeps its pane; `upgrade` with `force` stops it, installs, and the new daemon answers with the bundled version; `uninstall` stops the daemon and leaves no prefix, no runtime directory and no process.
- Every `BootstrapError` names its stage and carries the remote's stderr.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test remote_launch` passes every pure case, `timeout 900 cargo xtask claims verify --task remote-launch` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
